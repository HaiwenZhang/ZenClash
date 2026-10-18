use std::path::{Path, PathBuf};

use zenclash_core::{ControlledConfigStore, CoreApplyOutcome, CoreSession, PreparedBackupRestore};

use super::recovery::{rollback_after_runtime_accept, rollback_restore};
use super::{BackupRestoration, BackupRestoreReceipt, state::LoadedBackupState};

struct PreparedBackupConfig {
    runtime_identity: ExpectedBackupRuntime,
    service: Option<zenclash_core::PreparedServiceBackupConfig>,
}

struct ExpectedBackupRuntime {
    binding_generation: u64,
    accepted_config_generation: u64,
}

impl ExpectedBackupRuntime {
    fn capture(session: &CoreSession) -> Self {
        Self {
            binding_generation: session.runtime_descriptor().binding_generation(),
            accepted_config_generation: session.generation(),
        }
    }
    fn is_current(&self, session: &CoreSession) -> bool {
        self.binding_generation == session.runtime_descriptor().binding_generation()
            && self.accepted_config_generation == session.generation()
    }
}

impl BackupRestoration {
    /// Validates and prepares before admission; admitted completion survives waiter cancellation.
    pub(crate) async fn restore_archive(
        &self,
        archive: PathBuf,
    ) -> Result<BackupRestoreReceipt, String> {
        let manager = self.manager.clone();
        let prepared = tokio::task::spawn_blocking(move || manager.prepare_restore(archive))
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "backup.errors.validation_task",
                    &[("error", error.to_string())],
                )
            })?
            .map_err(|error| error.to_string())?;
        let (prepared, application) = self.prepare_backup_restore(prepared).await?;
        let admission = self
            .application_commands
            .session
            .begin_backup_restore()
            .await
            .map_err(|error| error.to_string())?;
        let restoration = self.clone();
        // Admission must immediately transfer to the independent completion owner.
        tokio::spawn(async move {
            restoration
                .restore_admitted(prepared, admission, application)
                .await
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.state_task", &[("error", error.to_string())])
        })?
    }
    async fn prepare_backup_restore(
        &self,
        prepared: zenclash_core::PreparedBackupRestore,
    ) -> Result<(zenclash_core::PreparedBackupRestore, PreparedBackupConfig), String> {
        let runtime_identity = ExpectedBackupRuntime::capture(&self.application_commands.session);
        let (prepared, service) = match self.application_commands.service_manager.as_ref() {
            Some(manager) => manager
                .prepare_backup_restore(prepared)
                .await
                .map_err(super::super::service_failure_message)?,
            None => (prepared, None),
        };
        Ok((
            prepared,
            PreparedBackupConfig {
                runtime_identity,
                service,
            },
        ))
    }

    async fn reload_backup_config(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
        prepared: PreparedBackupConfig,
        previous: Option<zenclash_core::CoreRestoreSnapshot>,
    ) -> Result<CoreApplyOutcome, String> {
        if !prepared
            .runtime_identity
            .is_current(&self.application_commands.session)
        {
            return Err(zenclash_i18n::text("core_page.service.stale"));
        }
        if let Some(prepared) = prepared.service {
            let manager = self
                .application_commands
                .service_manager
                .as_ref()
                .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
            let previous =
                previous.ok_or_else(|| zenclash_i18n::text("core_page.service.no_snapshot"))?;
            let outcome = manager
                .apply_backup_config(&controlled, &previous, prepared)
                .await
                .map_err(super::super::service_failure_message)?;
            return self
                .application_commands
                .accept_service_config_outcome(outcome)
                .map_err(|error| error.to_string());
        }
        self.application_commands
            .reload_with_overrides(controlled, path, overrides)
            .await
    }

    async fn restore_admitted(
        &self,
        prepared: PreparedBackupRestore,
        admission: zenclash_core::CoreBackupAdmission,
        application: PreparedBackupConfig,
    ) -> Result<BackupRestoreReceipt, String> {
        let commands = &self.application_commands;
        let file_count = prepared.file_count();
        let payload_bytes = prepared.payload_bytes();
        let session = commands.session().clone();
        let transaction = tokio::task::spawn_blocking(move || {
            prepared
                .activate_for_session(&session)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "backup.errors.activation_task",
                &[("error", error.to_string())],
            )
        })??;

        let root = self.manager.data_root().to_path_buf();
        let load_root = root.clone();
        let (transaction, loaded) = tokio::task::spawn_blocking(move || {
            let loaded = LoadedBackupState::read(&load_root, &transaction);
            (transaction, loaded)
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.state_task", &[("error", error.to_string())])
        })?;
        let loaded = match loaded {
            Ok(state) => state,
            Err(error) => return rollback_restore(transaction, error).await,
        };
        let overrides = loaded.enabled_overrides();
        let previous_runtime_version = commands.session().generation();
        let runtime_version = match self
            .reload_backup_config(
                loaded.controlled_store().clone(),
                loaded.active_profile_path(),
                overrides,
                application,
                transaction.previous_runtime_snapshot(),
            )
            .await
        {
            Ok(outcome) => outcome.generation,
            Err(error) => {
                let reason = zenclash_i18n::text_with(
                    "backup.errors.core_rejected",
                    &[
                        ("core", commands.kind().display_name().to_owned()),
                        ("error", error),
                    ],
                );
                if commands.session().generation() != previous_runtime_version {
                    return rollback_after_runtime_accept(
                        transaction,
                        commands.clone(),
                        root,
                        reason,
                        &admission,
                    )
                    .await;
                }
                return rollback_restore(transaction, reason).await;
            }
        };
        // Optional controller facts are domain observation, not page loading or a commit condition.
        let runtime_config = commands.client().runtime_config().await.ok();
        let (transaction, committed) = tokio::task::spawn_blocking(move || {
            let mut transaction = transaction;
            let committed = transaction.commit_in_place();
            (transaction, committed)
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.commit_task", &[("error", error.to_string())])
        })?;
        if let Err(error) = committed {
            return rollback_after_runtime_accept(
                transaction,
                commands.clone(),
                root,
                error.to_string(),
                &admission,
            )
            .await;
        }
        let cleanup_warning = tokio::task::spawn_blocking(move || transaction.commit())
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "backup.errors.commit_task",
                    &[("error", error.to_string())],
                )
            })?
            .err()
            .map(|error| error.to_string());
        Ok(loaded.committed(
            self.manager.clone(),
            runtime_version,
            runtime_config,
            file_count,
            payload_bytes,
            cleanup_warning,
        ))
    }
}
