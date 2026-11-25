//! Profile application commands, retained receipts, and localized outcome explanations.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreLifecyclePhase, EffectiveConfigIntent, ProfileApplication,
    ProfileApplyOutcome, ProfileChange, ProfileStore, RemoteProfileOptions,
};

use super::{ApplicationCommands, recovery_record::ServiceRuntimeFacts};

/// Result of deleting a profile from the catalog.
pub struct DeletedProfileOutcome {
    /// Whether the removed entry was the active profile.
    pub was_active: bool,
}

/// An accepted profile command result retaining the complete core receipt.
pub struct ProfileReceipt {
    path: PathBuf,
    name: String,
    runtime_version: Option<u64>,
    retained_outcome: Arc<ProfileApplyOutcome>,
}

impl ProfileReceipt {
    /// Returns the managed profile file path on disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the profile's display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the runtime generation if the profile was applied to the core.
    #[must_use]
    pub fn runtime_version(&self) -> Option<u64> {
        self.runtime_version
    }

    /// Returns a localized warning when the profile was durably saved but
    /// runtime confirmation remains pending.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
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

/// Localized failure from a profile command, retaining the complete core outcome.
#[derive(Debug)]
pub struct ProfileFailure {
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

impl std::error::Error for ProfileFailure {}

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
    /// Imports a local YAML profile and applies it through the serialized core session.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when validation, persistence, or runtime
    /// acceptance fails.
    pub async fn import_local(
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

    /// Downloads, stores, and applies a remote subscription profile.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when download, validation, persistence, or
    /// runtime acceptance fails.
    pub async fn add_remote(
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

    /// Activates an existing managed profile by ID.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when the profile is missing, invalid, or
    /// rejected by the core.
    pub async fn activate(
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

    /// Refreshes an existing remote subscription profile from its upstream URL.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when download, validation, persistence, or
    /// runtime acceptance fails.
    pub async fn update_remote(
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

    /// Persists an optimistic-concurrency YAML edit to a managed profile and
    /// applies it when active.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when the expected content changed, YAML
    /// validation fails, or runtime acceptance fails.
    pub async fn edit_yaml(
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

    /// Deletes an inactive or sole remaining profile and reconciles the active
    /// runtime cache when the sole active profile is removed.
    ///
    /// # Errors
    ///
    /// Returns an error if data admission or store deletion fails.
    pub async fn delete_profile(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        id: String,
        fallback_profile: Option<PathBuf>,
    ) -> Result<DeletedProfileOutcome, String> {
        let admission = self
            .session
            .begin_data_application()
            .await
            .map_err(|error| error.to_string())?;
        let commands = self.clone();
        tokio::spawn(async move {
            let admission = admission;
            let was_active =
                tokio::task::spawn_blocking(move || store.delete_sole_or_inactive(&id))
                    .await
                    .map_err(|error| {
                        zenclash_i18n::text_with(
                            "profiles.errors.delete_task",
                            &[("error", error.to_string())],
                        )
                    })?
                    .map_err(|error| error.to_string())?;
            if !was_active {
                return Ok(DeletedProfileOutcome { was_active: false });
            }
            let overrides = commands.enabled_overrides().await.unwrap_or_default();
            let running = !commands.session.is_offline_recovery()
                && commands.session.lifecycle_snapshot().phase != CoreLifecyclePhase::Stopped;
            if running
                && let Some(fallback) = fallback_profile.clone()
                && commands
                    .apply_config_admitted(
                        &admission,
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: fallback,
                            overrides: overrides.clone(),
                        },
                    )
                    .await
                    .is_ok()
            {
                return Ok(DeletedProfileOutcome { was_active: true });
            }
            let kind = commands.kind();
            let generation = commands
                .session
                .clear_committed_profile(fallback_profile.clone(), overrides.clone())
                .await;
            commands.recovery_record.accept_generation(generation);
            let cache_result = tokio::task::spawn_blocking(move || match fallback_profile {
                Some(fallback) => controlled
                    .materialize_with_overrides_for_core(&fallback, &overrides, kind)
                    .map(|_| ()),
                None => controlled.clear_runtime_cache(),
            })
            .await;
            if let Ok(Err(error)) = cache_result {
                tracing::warn!(
                    %error,
                    "failed to reset runtime cache after deleting sole active profile"
                );
            }
            Ok(DeletedProfileOutcome { was_active: true })
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.delete_task",
                &[("error", error.to_string())],
            )
        })?
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
            // Keep override reads and application inside the same data transaction.
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

    /// Publishes a synthetic [`ProfileApplyOutcome`] directly into the shared
    /// recovery record (test helper).
    ///
    /// # Errors
    ///
    /// Returns [`ProfileFailure`] when `outcome` represents a rejected,
    /// rolled-back, or unconfirmed profile transition.
    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_test_outcome(
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
