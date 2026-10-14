use std::path::PathBuf;

use zenclash_core::{
    CoreApplyOutcome, CoreSessionError, EffectiveConfigIntent, YamlOverrideCatalog,
};

use super::{
    OverrideApplication, OverrideApplicationFailure, OverrideApplicationReceipt,
    OverrideFailureKind,
};

pub(super) enum OverrideChange {
    Import {
        paths: Vec<PathBuf>,
    },
    ReplaceCatalog {
        expected: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
    },
    DeleteDisabled {
        id: String,
    },
    Reapply,
}

impl OverrideApplication {
    pub(super) async fn complete(
        &self,
        change: OverrideChange,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        let admission = self
            .application_commands
            .session
            .begin_data_application()
            .await
            .map_err(|error| OverrideApplicationFailure {
                message: error.to_string(),
                kind: OverrideFailureKind::Rejected,
                current_catalog: None,
                runtime_unconfirmed: false,
            })?;
        let application = self.clone();
        // No await separates admission from transfer to the independent completion owner.
        tokio::spawn(async move {
            let _admission = admission;
            application.apply_change(change).await
        })
        .await
        .map_err(|error| OverrideApplicationFailure {
            message: zenclash_i18n::text_with(
                "overrides.errors.change_workflow",
                &[("error", error.to_string())],
            ),
            kind: OverrideFailureKind::CompensationRequired,
            current_catalog: None,
            runtime_unconfirmed: true,
        })?
    }

    async fn apply_change(
        &self,
        change: OverrideChange,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        match change {
            OverrideChange::Import { paths } => self.import_admitted(paths).await,
            OverrideChange::ReplaceCatalog { expected, next } => {
                self.replace_admitted(expected, next).await
            }
            OverrideChange::DeleteDisabled { id } => {
                let store = self.store.clone();
                let result = tokio::task::spawn_blocking(move || store.delete_disabled(&id))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()));
                match result {
                    Ok(catalog) => Ok(OverrideApplicationReceipt {
                        catalog,
                        runtime: None,
                        imported_count: 0,
                    }),
                    Err(error) => Err(self
                        .failure(error, OverrideFailureKind::Rejected, false)
                        .await),
                }
            }
            OverrideChange::Reapply => {
                let catalog =
                    self.read_catalog()
                        .await
                        .map_err(|error| OverrideApplicationFailure {
                            message: error,
                            kind: OverrideFailureKind::Rejected,
                            current_catalog: None,
                            runtime_unconfirmed: false,
                        })?;
                let generation = self.application_commands.session.generation();
                match self.apply_runtime(&catalog).await {
                    Ok(runtime) => Ok(OverrideApplicationReceipt {
                        catalog,
                        runtime,
                        imported_count: 0,
                    }),
                    Err(error) => Err(self
                        .failure(
                            error,
                            OverrideFailureKind::Rejected,
                            self.application_commands.session.generation() != generation,
                        )
                        .await),
                }
            }
        }
    }

    async fn import_admitted(
        &self,
        paths: Vec<PathBuf>,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        let store = self.store.clone();
        let imported = match tokio::task::spawn_blocking(move || store.import_paths(paths)).await {
            Ok(Ok(imported)) => imported,
            Ok(Err(error)) => {
                let kind = if matches!(error, zenclash_core::YamlOverrideError::Transaction(_)) {
                    OverrideFailureKind::CompensationRequired
                } else {
                    OverrideFailureKind::Rejected
                };
                return Err(self.failure(error.to_string(), kind, false).await);
            }
            Err(error) => {
                return Err(self
                    .failure(
                        error.to_string(),
                        OverrideFailureKind::CompensationRequired,
                        false,
                    )
                    .await);
            }
        };
        let catalog = match self.read_catalog().await {
            Ok(catalog) => catalog,
            Err(error) => return Err(self.compensate_import(imported, error, false).await),
        };
        let generation = self.application_commands.session.generation();
        match self.apply_runtime(&catalog).await {
            Ok(runtime) => Ok(OverrideApplicationReceipt {
                catalog,
                runtime,
                imported_count: imported.len(),
            }),
            Err(error) => Err(self
                .compensate_import(
                    imported,
                    error,
                    self.application_commands.session.generation() != generation,
                )
                .await),
        }
    }

    async fn replace_admitted(
        &self,
        expected: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        let store = self.store.clone();
        let before = expected.clone();
        let persisted = next.clone();
        let saved = tokio::task::spawn_blocking(move || store.replace_catalog(&before, &persisted))
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
        if let Err(error) = saved {
            return Err(self
                .failure(error, OverrideFailureKind::Rejected, false)
                .await);
        }
        let generation = self.application_commands.session.generation();
        match self.apply_runtime(&next).await {
            Ok(runtime) => Ok(OverrideApplicationReceipt {
                catalog: next,
                runtime,
                imported_count: 0,
            }),
            Err(error) => Err(self
                .compensate_catalog(
                    next,
                    expected,
                    error,
                    self.application_commands.session.generation() != generation,
                )
                .await),
        }
    }

    pub(super) async fn read_catalog(&self) -> Result<YamlOverrideCatalog, String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.load())
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    async fn apply_runtime(
        &self,
        catalog: &YamlOverrideCatalog,
    ) -> Result<Option<CoreApplyOutcome>, String> {
        match self
            .application_commands
            .apply_config(
                &self.controlled_config,
                EffectiveConfigIntent::ReapplyCurrent {
                    overrides: self.store.enabled_paths(catalog),
                },
            )
            .await
        {
            Ok(outcome) => Ok(Some(outcome)),
            Err(CoreSessionError::NoCommittedProfile) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) async fn failure(
        &self,
        message: String,
        kind: OverrideFailureKind,
        runtime_unconfirmed: bool,
    ) -> OverrideApplicationFailure {
        OverrideApplicationFailure {
            message,
            kind,
            current_catalog: self.read_catalog().await.ok(),
            runtime_unconfirmed,
        }
    }
}
