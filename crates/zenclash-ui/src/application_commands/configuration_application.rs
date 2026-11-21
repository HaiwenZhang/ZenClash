//! Effective configuration commands and shared acceptance of their completion facts.

use super::{ApplicationCommands, recovery_record::ServiceRuntimeFacts, service_failure_message};
use std::path::{Path, PathBuf};
use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, CoreDataApplicationAdmission, CoreSessionError,
    EffectiveConfigIntent, ServiceManagerError, YamlOverrideStore,
};

impl ApplicationCommands {
    pub(super) async fn enabled_overrides(&self) -> Result<Vec<PathBuf>, String> {
        let store = self.overrides.clone();
        tokio::task::spawn_blocking(move || {
            let store = match store {
                Some(store) => store,
                None => YamlOverrideStore::discover()?,
            };
            store.load_enabled_paths()
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.override_read_task",
                &[("error", error.to_string())],
            )
        })?
        .map_err(|error| error.to_string())
    }

    pub(super) async fn reload_with_overrides_admitted(
        &self,
        admission: &CoreDataApplicationAdmission,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyOutcome, String> {
        self.apply_config_admitted(
            admission,
            &controlled,
            EffectiveConfigIntent::ActivateProfile {
                profile: path.to_path_buf(),
                overrides,
            },
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub(super) fn accept_service_config_outcome(
        &self,
        outcome: zenclash_core::ServiceConfigOutcome,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let outcome = match outcome {
            zenclash_core::ServiceConfigOutcome::Local(core) => {
                self.record_accepted_runtime(&core);
                return Ok(core);
            }
            zenclash_core::ServiceConfigOutcome::Service(outcome) => outcome,
        };
        if let Some(core) = outcome.core() {
            if let Some(facts) = ServiceRuntimeFacts::from_profile_or_config(&outcome) {
                self.recovery_record
                    .accept_service_runtime(facts, self.session.generation());
            }
            return Ok(*core);
        }
        Err(CoreSessionError::Config(
            zenclash_core::ControlledConfigError::Transaction(match outcome.capture() {
                zenclash_core::CaptureOutcome::RolledBack { failure, .. }
                | zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } => failure.clone(),
                _ => zenclash_i18n::text("core_page.service.unknown"),
            }),
        ))
    }

    pub(crate) async fn reapply_current(
        &self,
        controlled: ControlledConfigStore,
    ) -> Result<Option<CoreApplyOutcome>, String> {
        let admission = self
            .session
            .begin_data_application()
            .await
            .map_err(|error| error.to_string())?;
        let commands = self.clone();
        tokio::spawn(async move {
            // Keep override discovery and reapplication inside the same data transaction.
            let _admission = admission;
            let overrides = commands.enabled_overrides().await?;
            match commands
                .run_config_application(
                    &controlled,
                    EffectiveConfigIntent::ReapplyCurrent { overrides },
                )
                .await
            {
                Ok(outcome) => Ok(Some(outcome)),
                Err(CoreSessionError::NoCommittedProfile) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    #[cfg(test)]
    pub(crate) async fn reapply_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        overrides: Vec<PathBuf>,
    ) -> Result<Option<CoreApplyOutcome>, String> {
        match self
            .apply_config(
                &controlled,
                EffectiveConfigIntent::ReapplyCurrent { overrides },
            )
            .await
        {
            Ok(outcome) => Ok(Some(outcome)),
            Err(CoreSessionError::NoCommittedProfile) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) async fn apply_controlled_patch(
        &self,
        controlled: &ControlledConfigStore,
        profile: PathBuf,
        patch: serde_json::Value,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let admission = self.session.begin_data_application().await?;
        let commands = self.clone();
        let controlled = controlled.clone();
        tokio::spawn(async move {
            // Keep override discovery and patch application inside the same data transaction.
            let _admission = admission;
            let overrides = commands.enabled_overrides().await.map_err(|error| {
                CoreSessionError::Config(zenclash_core::ControlledConfigError::Transaction(error))
            })?;
            commands
                .run_config_application(
                    &controlled,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch,
                        overrides,
                    },
                )
                .await
        })
        .await
        .map_err(|_| {
            CoreSessionError::Config(zenclash_core::ControlledConfigError::Transaction(
                zenclash_i18n::text("core_page.service.pending"),
            ))
        })?
    }

    #[cfg(test)]
    pub(crate) async fn apply_config(
        &self,
        controlled: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let admission = self.session.begin_data_application().await?;
        self.apply_config_admitted(&admission, controlled, intent)
            .await
    }

    pub(super) async fn apply_config_admitted(
        &self,
        admission: &CoreDataApplicationAdmission,
        controlled: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let commands = self.clone();
        let controlled = controlled.clone();
        let admission = admission.clone();
        tokio::spawn(async move {
            let _admission = admission;
            commands.run_config_application(&controlled, intent).await
        })
        .await
        .map_err(|_| {
            CoreSessionError::Config(zenclash_core::ControlledConfigError::Transaction(
                zenclash_i18n::text("core_page.service.pending"),
            ))
        })?
    }

    async fn run_config_application(
        &self,
        controlled: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        if let Some(manager) = self.service_manager.as_ref()
            && let Some(outcome) = manager
                .try_apply_service_config(controlled, intent.clone())
                .await
                .map_err(|error| match error {
                    ServiceManagerError::Runtime(cause) => *cause,
                    error => {
                        CoreSessionError::Config(zenclash_core::ControlledConfigError::Transaction(
                            service_failure_message(error),
                        ))
                    }
                })?
        {
            return self.accept_service_config_outcome(outcome);
        }
        let outcome = self.session.apply(controlled, intent).await?;
        self.record_accepted_runtime(&outcome);
        Ok(outcome)
    }

    pub(super) fn record_accepted_runtime(&self, outcome: &CoreApplyOutcome) {
        self.recovery_record.accept_runtime(outcome);
    }
}
