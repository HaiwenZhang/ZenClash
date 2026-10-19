//! Profile application commands, retained receipts and localized outcome explanations.

use super::{ApplicationCommands, recovery_record::ServiceRuntimeFacts};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use zenclash_core::{
    ControlledConfigStore, CoreKind, ProfileApplication, ProfileApplyOutcome, ProfileChange,
    ProfileStore, RemoteProfileOptions,
};

/// An accepted business result, retaining the complete core receipt.
pub(crate) struct ProfileReceipt {
    path: PathBuf,
    name: String,
    runtime_version: Option<u64>,
    retained_outcome: Arc<ProfileApplyOutcome>,
}

impl ProfileReceipt {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn runtime_version(&self) -> Option<u64> {
        self.runtime_version
    }

    pub(crate) fn warning(&self) -> Option<String> {
        matches!(
            self.retained_outcome.as_ref(),
            ProfileApplyOutcome::CommittedButRuntimeUnknown { .. }
        )
        .then(|| {
            failure_message(
                self.retained_outcome.as_ref(),
                ChangeContext::Selection,
                CoreKind::Mihomo,
            )
        })
    }
}

#[derive(Debug)]
pub(crate) struct ProfileFailure {
    message: String,
    // Preserve the complete failure including attempted/last-known-good revisions
    // and rollback cause, independently of a page's displayed error string.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retains the complete outcome and its resources independently of the display string; tests inspect the retained business facts"
        )
    )]
    pub(super) retained_outcome: Option<Arc<ProfileApplyOutcome>>,
}

impl fmt::Display for ProfileFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for ProfileFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            retained_outcome: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ChangeContext {
    Selection,
    Update,
    Editor,
}

impl ApplicationCommands {
    pub(crate) async fn import_local(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        source: PathBuf,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.apply(
            store,
            controlled,
            move |overrides| ProfileChange::ImportLocal { source, overrides },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn add_remote(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.apply(
            store,
            controlled,
            move |overrides| ProfileChange::AddRemote {
                name,
                url,
                user_agent,
                options,
                overrides,
            },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn activate(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.apply(
            store,
            controlled,
            move |overrides| ProfileChange::ActivateExisting { id, overrides },
            ChangeContext::Selection,
        )
        .await
    }

    pub(crate) async fn update_remote(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.apply(
            store,
            controlled,
            move |overrides| ProfileChange::UpdateRemote { id, overrides },
            ChangeContext::Update,
        )
        .await
    }

    pub(crate) async fn edit_yaml(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
        expected_payload: String,
        new_payload: String,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.apply(
            store,
            controlled,
            move |overrides| ProfileChange::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            },
            ChangeContext::Editor,
        )
        .await
    }

    async fn apply(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        change: impl FnOnce(Vec<PathBuf>) -> ProfileChange + Send + 'static,
        context: ChangeContext,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let admission = self
            .session
            .begin_data_application()
            .await
            .map_err(|error| ProfileFailure::from(error.to_string()))?;
        let commands = self.clone();
        tokio::spawn(async move {
            // Keep override discovery and application inside the same data transaction.
            let _admission = admission;
            let change = change(commands.enabled_overrides().await?);
            let application = ProfileApplication::new(store, controlled, commands.session.clone());
            let (outcome, service) = if let Some(manager) = &commands.service_manager {
                application.apply_with_service(manager, change).await
            } else {
                (application.apply(change).await, None)
            };
            let service_facts = service
                .as_ref()
                .and_then(ServiceRuntimeFacts::from_profile_or_config);
            commands.finish_apply(Arc::new(outcome), context, service_facts)
        })
        .await
        .map_err(|_| ProfileFailure::from(zenclash_i18n::text("core_page.service.pending")))?
    }

    pub(super) fn finish_apply(
        &self,
        outcome: Arc<ProfileApplyOutcome>,
        context: ChangeContext,
        service_facts: Option<ServiceRuntimeFacts>,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.recovery_record
            .accept_profile_completion(&outcome, service_facts, &self.session);
        match outcome.as_ref() {
            ProfileApplyOutcome::Applied {
                profile,
                path,
                runtime_version,
                ..
            }
            | ProfileApplyOutcome::CommittedButRuntimeUnknown {
                profile,
                path,
                runtime_version,
                ..
            } => Ok(ProfileReceipt {
                path: path.clone(),
                name: profile.name.clone(),
                runtime_version: Some(*runtime_version),
                retained_outcome: outcome,
            }),
            ProfileApplyOutcome::Stored { profile, path, .. } => Ok(ProfileReceipt {
                path: path.clone(),
                name: profile.name.clone(),
                runtime_version: None,
                retained_outcome: outcome,
            }),
            _ => {
                let message = failure_message(outcome.as_ref(), context, self.kind());
                Err(ProfileFailure {
                    message,
                    retained_outcome: Some(outcome),
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn publish_test_outcome(
        &self,
        outcome: ProfileApplyOutcome,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.finish_apply(Arc::new(outcome), ChangeContext::Selection, None)
    }
}

fn failure_message(
    outcome: &ProfileApplyOutcome,
    context: ChangeContext,
    kind: CoreKind,
) -> String {
    let core = kind.display_name().to_owned();
    match outcome {
        ProfileApplyOutcome::Rejected { cause, .. } => cause.to_string(),
        ProfileApplyOutcome::RolledBack { cause, .. } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Selection => "profiles.errors.rejected_rolled_back",
                ChangeContext::Update => "profiles.errors.update_rejected_rolled_back",
                ChangeContext::Editor => "overrides.errors.editor_rejected_rolled_back",
            },
            &[("core", core), ("error", cause.to_string())],
        ),
        ProfileApplyOutcome::RuntimeUnknown { cause, .. } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Editor => "overrides.errors.editor_runtime_unknown",
                _ => "profiles.errors.runtime_unknown",
            },
            &[("core", core), ("error", cause.to_string())],
        ),
        ProfileApplyOutcome::CommittedButRuntimeUnknown { cause, .. } => zenclash_i18n::text_with(
            "profiles.recovery.saved_pending",
            &[("error", cause.to_string())],
        ),
        ProfileApplyOutcome::PersistedButRuntimeUnknown {
            cause, rollback, ..
        } => zenclash_i18n::text_with(
            match context {
                ChangeContext::Selection => "profiles.errors.rejected_rollback_failed",
                ChangeContext::Update => "profiles.errors.update_rejected_rollback_failed",
                ChangeContext::Editor => "overrides.errors.editor_rejected_rollback_failed",
            },
            &[
                ("core", core),
                ("error", cause.to_string()),
                ("rollback", rollback.to_string()),
            ],
        ),
        _ => zenclash_i18n::text("profiles.errors.not_applied"),
    }
}
