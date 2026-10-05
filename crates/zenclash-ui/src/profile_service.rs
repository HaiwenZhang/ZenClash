//! Shared profile commands for pages, the tray, and automatic updates.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, CoreCommittedProfileSnapshot,
    CoreInitializationOutcome, CoreKind, CoreRestoreSnapshot, CoreSession, CoreSessionError,
    EffectiveConfigIntent, MihomoClient, ProfileApplication, ProfileApplyOutcome, ProfileChange,
    ProfileRecovery, ProfileStore, RemoteProfileOptions, ServiceHealthKind, ServiceManager,
    ServiceManagerError, ServiceManagerSnapshot, ServiceTunOutcome, ServiceTunRequest,
    YamlOverrideStore,
};

/// Shared application-level profile workflow and typed recovery context.
///
/// Clones share the same recovery record. Runtime ordering remains owned by
/// `CoreSession`, and repository transactions remain owned by `ProfileApplication`.
#[derive(Clone)]
pub struct ProfileService {
    session: CoreSession,
    overrides: Option<YamlOverrideStore>,
    failure: Arc<Mutex<ProfileRecoveryState>>,
    service_manager: Option<ServiceManager>,
    maintenance_command: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct ProfileRecoveryState {
    runtime_result_version: Option<u64>,
    latest_failure: Option<Arc<ProfileApplyOutcome>>,
    unresolved: Option<Arc<ProfileApplyOutcome>>,
    service_tun_pending: Option<u64>,
    service_tun_warning: Option<String>,
    service_maintenance: Option<Arc<zenclash_core::ServiceMaintenancePreparation>>,
    service_recovery: Option<Arc<zenclash_core::ServiceMaintenancePreparation>>,
}

impl ProfileRecoveryState {
    fn accept_version(&mut self, version: u64) -> bool {
        if self
            .runtime_result_version
            .is_some_and(|published| version <= published)
        {
            return false;
        }
        self.runtime_result_version = Some(version);
        true
    }

    fn clear_failure(&mut self) {
        self.unresolved = None;
        self.latest_failure = None;
        self.service_tun_pending = None;
        self.service_tun_warning = None;
    }

    fn accept_local_recovery(&mut self, version: u64) {
        if self.accept_version(version) {
            self.clear_failure();
        }
    }

    fn pending_finalization(&self) -> Option<u64> {
        match self.unresolved.as_deref() {
            Some(ProfileApplyOutcome::CommittedButRuntimeUnknown {
                runtime_version, ..
            }) => Some(*runtime_version),
            _ => self.service_tun_pending,
        }
    }

    fn confirm_pending(&mut self, version: u64) -> bool {
        if self.pending_finalization() != Some(version) {
            return false;
        }
        self.unresolved = None;
        self.latest_failure = None;
        self.service_tun_pending = None;
        true
    }
}

/// An accepted business result, retaining the complete core receipt.
pub(crate) struct ProfileReceipt {
    path: PathBuf,
    name: String,
    runtime_version: Option<u64>,
    _outcome: Arc<ProfileApplyOutcome>,
}

pub(crate) struct PreparedBackupApplication {
    runtime_identity: (u64, u64),
    service: Option<zenclash_core::PreparedServiceBackupConfig>,
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
            self._outcome.as_ref(),
            ProfileApplyOutcome::CommittedButRuntimeUnknown { .. }
        )
        .then(|| {
            failure_message(
                self._outcome.as_ref(),
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
    _outcome: Option<Arc<ProfileApplyOutcome>>,
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
            _outcome: None,
        }
    }
}

#[derive(Clone, Copy)]
enum ChangeContext {
    Selection,
    Update,
    Editor,
}

impl ProfileService {
    /// Composes existing core services without creating another runtime owner.
    #[must_use]
    pub fn new(session: CoreSession, overrides: Option<YamlOverrideStore>) -> Self {
        Self {
            session,
            overrides,
            failure: Arc::new(Mutex::new(ProfileRecoveryState::default())),
            service_manager: None,
            maintenance_command: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn with_service_manager(mut self, manager: ServiceManager) -> Self {
        self.service_manager = Some(manager);
        self
    }

    pub(crate) fn service_state(&self) -> Option<ServiceManagerSnapshot> {
        self.service_manager.as_ref().map(ServiceManager::snapshot)
    }

    pub(crate) fn service_updates(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<ServiceManagerSnapshot>> {
        self.service_manager.as_ref().map(ServiceManager::subscribe)
    }

    pub(crate) fn request_service_tun(&self) -> Result<ServiceTunRequest, String> {
        let manager = self
            .service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        if self.session.runtime_descriptor().backend() == zenclash_core::CoreRuntimeBackend::Local
            && let Some(preparation) = self.service_local_recovery()
            && let Some(recovery) = preparation.recovery()
            && recovery.runtime().generation() == self.session.generation()
        {
            return manager
                .request_enable_tun_after_recovery(recovery.runtime())
                .map_err(service_failure_message);
        }
        manager
            .request_enable_tun()
            .map_err(service_failure_message)
    }

    /// Captures a maintenance intent for the current runtime without performing I/O.
    ///
    /// # Errors
    /// Returns an error when maintenance is unsupported or the runtime is unresolved.
    pub fn request_service_maintenance(
        &self,
        operation: zenclash_core::ServiceOperation,
        fallback: Option<zenclash_core::MihomoLaunchConfig>,
    ) -> Result<zenclash_core::ServiceMaintenanceRequest, String> {
        self.service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?
            .request_maintenance(operation, fallback)
            .map_err(service_failure_message)
    }

    /// Resolves ordinary identity for direct Service maintenance before confirmation.
    /// Manager owns background discovery and its prepared progress.
    ///
    /// # Errors
    /// Returns unsupported/stale intent, overlapping command or executable discovery errors.
    pub async fn discover_service_maintenance(
        &self,
        operation: zenclash_core::ServiceOperation,
        project_root: PathBuf,
        preferred_binary: Option<PathBuf>,
    ) -> Result<zenclash_core::ServiceMaintenanceRequest, String> {
        self.service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?
            .discover_maintenance_request(operation, project_root, preferred_binary)
            .await
            .map_err(service_failure_message)
    }

    /// Prepares maintenance and retains its receipt independently of any page waiter.
    /// No administrator operation is submitted by this method.
    ///
    /// # Errors
    /// Returns runtime preparation errors; an unready receipt remains available to callers.
    pub async fn prepare_service_maintenance(
        &self,
        request: zenclash_core::ServiceMaintenanceRequest,
        store: &ControlledConfigStore,
    ) -> Result<Arc<zenclash_core::ServiceMaintenancePreparation>, String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let store = store.clone();
        let profiles = self.clone();
        let command = self
            .maintenance_command
            .clone()
            .try_lock_owned()
            .map_err(|_| service_failure_message(ServiceManagerError::Busy))?;
        tokio::spawn(async move {
            let _command = command;
            let preparation = Arc::new(
                manager
                    .prepare_maintenance(request, &store)
                    .await
                    .map_err(service_failure_message)?,
            );
            let mut state = profiles.failure.lock();
            if let Some(recovery) = preparation.recovery() {
                if recovery.runtime().ready()
                    && profiles.session.generation() == recovery.runtime().generation()
                {
                    state.accept_local_recovery(recovery.runtime().generation());
                }
                state.service_recovery = Some(preparation.clone());
            }
            state.service_maintenance = Some(preparation.clone());
            Ok(preparation)
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    /// Reads the latest maintenance preparation without I/O.
    pub fn service_maintenance_preparation(
        &self,
    ) -> Option<Arc<zenclash_core::ServiceMaintenancePreparation>> {
        self.failure.lock().service_maintenance.clone()
    }

    /// Reads the last Service-to-Local resources, preserved across later Local commands.
    /// The core receipt still checks identity and generation before any resource reuse.
    pub fn service_local_recovery(
        &self,
    ) -> Option<Arc<zenclash_core::ServiceMaintenancePreparation>> {
        self.failure.lock().service_recovery.clone()
    }

    /// Submits the latest preparation with the caller's explicit authorization choice.
    /// Keeps recovery resources after success, cancellation, or failure.
    ///
    /// # Errors
    /// Rejects replaced/foreign receipts, missing consent, stale runtime or native failure.
    pub async fn maintain_prepared_service(
        &self,
        preparation: Arc<zenclash_core::ServiceMaintenancePreparation>,
        allow_authorization: bool,
    ) -> Result<(), String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let command = self
            .maintenance_command
            .clone()
            .try_lock_owned()
            .map_err(|_| service_failure_message(ServiceManagerError::Busy))?;
        let current = self.failure.lock().service_maintenance.clone();
        if !current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &preparation))
        {
            return Err(service_failure_message(ServiceManagerError::Stale));
        }
        let preparation = if allow_authorization {
            preparation.as_ref().clone().with_authorization()
        } else {
            preparation.as_ref().clone()
        };
        let profiles = self.clone();
        tokio::spawn(async move {
            let _command = command;
            let maintenance = manager.maintain_prepared(&preparation).await;
            let recoverable = allow_authorization
                && preparation.operation() == zenclash_core::ServiceOperation::Repair
                && preparation.recovery().is_some()
                && maintenance.as_ref().err().is_none_or(|error| {
                    !error.is_native_outcome_unconfirmed()
                        && !matches!(
                            error,
                            ServiceManagerError::Stale
                                | ServiceManagerError::Closed
                                | ServiceManagerError::Busy
                                | ServiceManagerError::ConsentRequired
                        )
                });
            let recovery = if recoverable {
                match manager.restore_prepared_capture(&preparation).await {
                    Ok(outcome) => {
                        if let Some(tun) = outcome.tun()
                            && let Some(core) = tun.core()
                        {
                            profiles.record_service_tun(
                                core,
                                tun.commit_pending(),
                                outcome.warning().or(tun.recovery_warning()),
                            );
                        }
                        outcome.warning().map(str::to_owned)
                    }
                    Err(error) => Some(service_failure_message(error)),
                }
            } else {
                None
            };
            match (maintenance, recovery) {
                (Ok(()), None) => Ok(()),
                (Ok(()), Some(warning)) => Err(warning),
                (Err(error), None) => Err(service_failure_message(error)),
                (Err(error), Some(warning)) => {
                    Err(format!("{}; {warning}", service_failure_message(error)))
                }
            }
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    pub(crate) async fn enable_service_tun(
        &self,
        request: ServiceTunRequest,
    ) -> Result<ServiceTunOutcome, String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let profiles = self.clone();
        // Navigation can discard its waiter, but not acceptance of a durable
        // core receipt in the shared profile recovery record.
        tokio::spawn(async move {
            let outcome = manager
                .enable_tun(request)
                .await
                .map_err(service_failure_message)?;
            if let Some(core) = outcome.core() {
                profiles.record_service_tun(
                    core,
                    outcome.commit_pending(),
                    outcome.recovery_warning(),
                );
            }
            Ok(outcome)
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    pub(crate) fn session(&self) -> &CoreSession {
        &self.session
    }

    pub(crate) fn client(&self) -> &MihomoClient {
        self.session.client()
    }
    pub(crate) fn kind(&self) -> CoreKind {
        self.session.kind()
    }

    pub(crate) fn latest_recovery(&self) -> Option<ProfileRecovery> {
        match self.failure.lock().unresolved.as_deref() {
            Some(
                ProfileApplyOutcome::RuntimeUnknown { recovery, .. }
                | ProfileApplyOutcome::PersistedButRuntimeUnknown { recovery, .. },
            ) => Some(recovery.clone()),
            _ => None,
        }
    }

    pub(crate) fn pending_finalization(&self) -> Option<u64> {
        self.failure.lock().pending_finalization()
    }

    pub(crate) fn service_tun_warning(&self) -> Option<String> {
        self.failure.lock().service_tun_warning.clone()
    }

    pub(crate) async fn confirm_service_runtime(&self, version: u64) -> Result<(), String> {
        // The retained outcome identifies a durable profile commit. Ordinary mode
        // and settings changes can advance CoreSession without replacing that commit.
        // Native reconciliation verifies the service revision; only a newer outcome
        // may supersede this confirmation token.
        if self.pending_finalization() != Some(version) {
            return Err(zenclash_i18n::text("profiles.recovery.confirm_stale"));
        }
        self.client()
            .reconcile_service_runtime()
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "profiles.recovery.confirm_failed",
                    &[("error", error.to_string())],
                )
            })?;
        if !self.failure.lock().confirm_pending(version) {
            return Err(zenclash_i18n::text("profiles.recovery.confirm_stale"));
        }
        Ok(())
    }

    pub(crate) fn committed_profile(&self) -> CoreCommittedProfileSnapshot {
        self.session.committed_profile_snapshot()
    }

    pub(crate) fn is_current(&self, version: u64) -> bool {
        self.session.generation() == version
    }

    pub(crate) async fn enabled_overrides(&self) -> Result<Vec<PathBuf>, String> {
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

    pub(crate) async fn import_local(
        &self,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        source: PathBuf,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::ImportLocal { source, overrides },
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
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::AddRemote {
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
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::ActivateExisting { id, overrides },
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
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::UpdateRemote { id, overrides },
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
        let overrides = self.enabled_overrides().await?;
        self.apply(
            store,
            controlled,
            ProfileChange::EditYaml {
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
        change: ProfileChange,
        context: ChangeContext,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        let profiles = self.clone();
        tokio::spawn(async move {
            let application = ProfileApplication::new(store, controlled, profiles.session.clone());
            let (outcome, service) = if let Some(manager) = &profiles.service_manager {
                application.apply_with_service(manager, change).await
            } else {
                (application.apply(change).await, None)
            };
            let receipt = profiles.finish_apply(Arc::new(outcome), context);
            if let Some(service) = service
                && let Some(core) = service.core()
            {
                let capture_warning = match service.capture() {
                    zenclash_core::CaptureOutcome::RolledBack { failure, .. }
                    | zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } => {
                        Some(failure.as_str())
                    }
                    _ => None,
                };
                let warning = match (service.recovery_warning(), capture_warning) {
                    (Some(first), Some(second)) => Some(format!("{first}; {second}")),
                    (first, second) => first.or(second).map(str::to_owned),
                };
                profiles.record_profile_service_facts(
                    core,
                    service.commit_pending(),
                    warning.as_deref(),
                );
            }
            receipt
        })
        .await
        .map_err(|_| ProfileFailure::from(zenclash_i18n::text("core_page.service.pending")))?
    }

    fn finish_apply(
        &self,
        outcome: Arc<ProfileApplyOutcome>,
        context: ChangeContext,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.record_outcome(&outcome);
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
                _outcome: outcome,
            }),
            ProfileApplyOutcome::Stored { profile, path, .. } => Ok(ProfileReceipt {
                path: path.clone(),
                name: profile.name.clone(),
                runtime_version: None,
                _outcome: outcome,
            }),
            _ => {
                let message = failure_message(outcome.as_ref(), context, self.kind());
                Err(ProfileFailure {
                    message,
                    _outcome: Some(outcome),
                })
            }
        }
    }

    fn record_outcome(&self, outcome: &Arc<ProfileApplyOutcome>) {
        let version = match outcome.as_ref() {
            ProfileApplyOutcome::Applied {
                runtime_version, ..
            }
            | ProfileApplyOutcome::RolledBack {
                runtime_version, ..
            }
            | ProfileApplyOutcome::RuntimeUnknown {
                runtime_version, ..
            }
            | ProfileApplyOutcome::PersistedButRuntimeUnknown {
                runtime_version, ..
            }
            | ProfileApplyOutcome::CommittedButRuntimeUnknown {
                runtime_version, ..
            } => Some(*runtime_version),
            ProfileApplyOutcome::Stored { .. } | ProfileApplyOutcome::Rejected { .. } => None,
        };
        let mut state = self.failure.lock();
        if let Some(version) = version
            && !state.accept_version(version)
        {
            return;
        }
        match outcome.as_ref() {
            ProfileApplyOutcome::Applied { .. } => state.clear_failure(),
            ProfileApplyOutcome::RolledBack { .. } => {
                state.unresolved = None;
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::RuntimeUnknown { .. }
            | ProfileApplyOutcome::PersistedButRuntimeUnknown { .. }
            | ProfileApplyOutcome::CommittedButRuntimeUnknown { .. } => {
                state.unresolved = Some(Arc::clone(outcome));
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::Rejected { .. } => {
                state.latest_failure = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::Stored { .. } => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn publish_test_outcome(
        &self,
        outcome: ProfileApplyOutcome,
    ) -> Result<ProfileReceipt, ProfileFailure> {
        self.finish_apply(Arc::new(outcome), ChangeContext::Selection)
    }

    pub(crate) async fn reload_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyOutcome, String> {
        self.apply_config(
            &controlled,
            EffectiveConfigIntent::ActivateProfile {
                profile: path.to_path_buf(),
                overrides,
            },
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn prepare_backup_restore(
        &self,
        prepared: zenclash_core::PreparedBackupRestore,
    ) -> Result<
        (
            zenclash_core::PreparedBackupRestore,
            PreparedBackupApplication,
        ),
        String,
    > {
        let runtime_identity = (
            self.session.runtime_descriptor().binding_generation(),
            self.session.generation(),
        );
        let (prepared, service) = match self.service_manager.as_ref() {
            Some(manager) => manager
                .prepare_backup_restore(prepared)
                .await
                .map_err(service_failure_message)?,
            None => (prepared, None),
        };
        Ok((
            prepared,
            PreparedBackupApplication {
                runtime_identity,
                service,
            },
        ))
    }

    pub(crate) async fn reload_backup_config(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
        prepared: PreparedBackupApplication,
        previous: Option<zenclash_core::CoreRestoreSnapshot>,
    ) -> Result<CoreApplyOutcome, String> {
        if prepared.runtime_identity
            != (
                self.session.runtime_descriptor().binding_generation(),
                self.session.generation(),
            )
        {
            return Err(zenclash_i18n::text("core_page.service.stale"));
        }
        if let Some(prepared) = prepared.service {
            let manager = self
                .service_manager
                .as_ref()
                .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
            let previous =
                previous.ok_or_else(|| zenclash_i18n::text("core_page.service.no_snapshot"))?;
            let outcome = manager
                .apply_backup_config(&controlled, &previous, prepared)
                .await
                .map_err(service_failure_message)?;
            return self
                .accept_service_config_outcome(outcome)
                .map_err(|error| error.to_string());
        }
        self.reload_with_overrides(controlled, path, overrides)
            .await
    }

    fn accept_service_config_outcome(
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
            let capture_warning = match outcome.capture() {
                zenclash_core::CaptureOutcome::RolledBack { failure, .. }
                | zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } => {
                    Some(failure.as_str())
                }
                _ => None,
            };
            let warning = match (outcome.recovery_warning(), capture_warning) {
                (Some(first), Some(second)) => Some(format!("{first}; {second}")),
                (first, second) => first.or(second).map(str::to_owned),
            };
            self.record_service_tun(core, outcome.commit_pending(), warning.as_deref());
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

    pub(crate) async fn apply_config(
        &self,
        controlled: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let profiles = self.clone();
        let controlled = controlled.clone();
        tokio::spawn(async move {
            if let Some(manager) = profiles.service_manager.as_ref()
                && let Some(outcome) = manager
                    .try_apply_service_config(&controlled, intent.clone())
                    .await
                    .map_err(|error| match error {
                        ServiceManagerError::Runtime(cause) => *cause,
                        error => CoreSessionError::Config(
                            zenclash_core::ControlledConfigError::Transaction(
                                service_failure_message(error),
                            ),
                        ),
                    })?
            {
                return profiles.accept_service_config_outcome(outcome);
            }
            let outcome = profiles.session.apply(&controlled, intent).await?;
            profiles.record_accepted_runtime(&outcome);
            Ok(outcome)
        })
        .await
        .map_err(|_| {
            CoreSessionError::Config(zenclash_core::ControlledConfigError::Transaction(
                zenclash_i18n::text("core_page.service.pending"),
            ))
        })?
    }

    fn record_accepted_runtime(&self, outcome: &CoreApplyOutcome) {
        let mut state = self.failure.lock();
        if state.accept_version(outcome.generation) {
            state.clear_failure();
        }
    }

    pub(crate) fn record_service_initialization(&self, outcome: &CoreInitializationOutcome) {
        self.record_initialization_facts(outcome.saved(), outcome.commit_pending());
    }

    fn record_initialization_facts(&self, saved: Option<&CoreApplyOutcome>, commit_pending: bool) {
        if let Some(saved) = saved {
            // Pending acknowledgement is a distinct fact; a saved startup has
            // already accepted its source and cannot offer old-source rollback.
            self.record_service_tun(saved, commit_pending, None);
        }
    }

    fn record_profile_service_facts(
        &self,
        outcome: &CoreApplyOutcome,
        commit_pending: bool,
        recovery_warning: Option<&str>,
    ) {
        if self.session.generation() != outcome.generation {
            return;
        }
        let mut state = self.failure.lock();
        // Directory and capture facts are two receipts for the same accepted transaction.
        // Add capture facts without clearing its committed-but-unknown source receipt.
        if state.runtime_result_version == Some(outcome.generation) {
            state.service_tun_pending = commit_pending.then_some(outcome.generation);
            state.service_tun_warning = recovery_warning.map(str::to_owned);
        }
    }

    fn record_service_tun(
        &self,
        outcome: &CoreApplyOutcome,
        commit_pending: bool,
        recovery_warning: Option<&str>,
    ) {
        if self.session.generation() != outcome.generation {
            return;
        }
        let mut state = self.failure.lock();
        if state.accept_version(outcome.generation) {
            state.clear_failure();
            if commit_pending {
                state.service_tun_pending = Some(outcome.generation);
            }
            state.service_tun_warning = recovery_warning.map(str::to_owned);
        }
    }

    pub(crate) fn pending_backup_restore(&self) -> Option<u64> {
        self.session.pending_backup_restore()
    }

    pub(crate) async fn retry_backup_restore(&self) -> Result<CoreApplyOutcome, String> {
        let service = self.clone();
        // The accepted core result and shared recovery record have one completion
        // owner even when the foreground waiter disappears.
        tokio::spawn(async move {
            let outcome = service
                .session
                .retry_backup_restore()
                .await
                .map_err(|error| error.to_string())?;
            service.record_accepted_runtime(&outcome);
            Ok(outcome)
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.retry_task", &[("error", error.to_string())])
        })?
    }

    pub(crate) async fn restore_backup_snapshot(
        &self,
        controlled: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
        admission: &zenclash_core::CoreBackupAdmission,
    ) -> Result<CoreApplyOutcome, String> {
        let outcome = self
            .session
            .restore_backup_snapshot(controlled, snapshot, admission)
            .await
            .map_err(|error| error.to_string())?;
        self.record_accepted_runtime(&outcome);
        Ok(outcome)
    }
}

fn service_failure_message(error: ServiceManagerError) -> String {
    if error.is_authorization_cancelled() {
        return zenclash_i18n::text("core_page.service.cancelled");
    }
    if error.is_native_outcome_unconfirmed() {
        return zenclash_i18n::text("core_page.service.pending");
    }
    let key = match error {
        ServiceManagerError::Busy => "core_page.service.busy",
        ServiceManagerError::Stale | ServiceManagerError::Closed => "core_page.service.stale",
        ServiceManagerError::Unsupported => "core_page.service.unsupported",
        ServiceManagerError::ConsentRequired => "core_page.service.install_description",
        ServiceManagerError::Sources(_) => "core_page.service.bundle_missing",
        ServiceManagerError::Health(kind) => match kind {
            ServiceHealthKind::NotInstalled => "core_page.service.missing",
            ServiceHealthKind::Ready | ServiceHealthKind::Unknown => "core_page.service.unknown",
            ServiceHealthKind::Unavailable(_) => "core_page.service.unavailable",
            ServiceHealthKind::VersionMismatch => "core_page.service.incompatible",
        },
        ServiceManagerError::Runtime(error) => return error.to_string(),
        ServiceManagerError::Maintenance(_) => "core_page.service.failed",
        _ => "core_page.service.unknown",
    };
    zenclash_i18n::text(key)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn service_config_reapply_without_a_committed_profile_preserves_the_noop_result() {
        let (profiles, store) = maintenance_fixture();
        assert!(
            profiles
                .reapply_with_overrides(store.clone(), vec![])
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(profiles.session.generation(), 0);
        assert!(!store.root().exists());
        assert!(profiles.service_state().unwrap().health().is_none());
        assert!(profiles.pending_finalization().is_none());
        profiles.session.shutdown().await.unwrap();
    }

    fn maintenance_fixture() -> (ProfileService, ControlledConfigStore) {
        let home = std::env::temp_dir().join(format!(
            "zenclash-ui-maintenance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ControlledConfigStore::new(home.join("controlled"));
        let process =
            zenclash_core::MihomoProcess::prepare_stopped(zenclash_core::MihomoLaunchConfig {
                kind: CoreKind::Mihomo,
                binary: home.join("ordinary-mihomo"),
                config_file: home.join("missing-source.yaml"),
                home_dir: home,
                endpoint: zenclash_core::MihomoEndpoint::default(),
                controller_override: None,
            });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process).unwrap(),
        )
        .unwrap();
        let capture =
            zenclash_core::TrafficCaptureSession::new(session.clone(), store.clone(), None, None);
        let manager = ServiceManager::new(session.clone(), capture);
        (
            ProfileService::new(session, None).with_service_manager(manager),
            store,
        )
    }

    #[tokio::test]
    async fn maintenance_preparation_survives_page_owner_drop_and_consent_rejection() {
        let (page, store) = maintenance_fixture();
        let shared = page.clone();
        let request = page
            .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
            .unwrap();
        let preparation = page
            .prepare_service_maintenance(request, &store)
            .await
            .unwrap();
        drop(page);
        assert!(Arc::ptr_eq(
            &shared.service_maintenance_preparation().unwrap(),
            &preparation
        ));
        assert!(
            shared
                .maintain_prepared_service(preparation.clone(), false)
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &shared.service_maintenance_preparation().unwrap(),
            &preparation
        ));
        assert!(preparation.ready());
        assert!(shared.service_local_recovery().is_none());
        assert!(
            !store.root().exists(),
            "Local preparation performed filesystem writes"
        );
    }

    #[tokio::test]
    async fn replaced_maintenance_preparation_cannot_submit_native_work() {
        let (profiles, store) = maintenance_fixture();
        let request = profiles
            .request_service_maintenance(zenclash_core::ServiceOperation::Repair, None)
            .unwrap();
        let previous = profiles
            .prepare_service_maintenance(request, &store)
            .await
            .unwrap();
        let request = profiles
            .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
            .unwrap();
        let current = profiles
            .prepare_service_maintenance(request, &store)
            .await
            .unwrap();
        assert!(
            profiles
                .maintain_prepared_service(previous, true)
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &profiles.service_maintenance_preparation().unwrap(),
            &current
        ));
        assert_eq!(profiles.service_state().unwrap().revision(), 2);
        assert!(!store.root().exists());
    }

    #[tokio::test]
    async fn maintenance_business_command_rejects_overlap_before_preparation() {
        let (profiles, store) = maintenance_fixture();
        let shared = profiles.clone();
        let _command = profiles.maintenance_command.lock().await;
        let request = shared
            .request_service_maintenance(zenclash_core::ServiceOperation::Repair, None)
            .unwrap();
        assert!(
            shared
                .prepare_service_maintenance(request, &store)
                .await
                .is_err()
        );
        assert!(shared.service_maintenance_preparation().is_none());
        assert_eq!(shared.service_state().unwrap().revision(), 0);
    }

    #[tokio::test]
    async fn cancelled_maintenance_waiter_still_publishes_shared_preparation() {
        use std::{future::Future, task::Poll};

        let (profiles, store) = maintenance_fixture();
        let request = profiles
            .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
            .unwrap();
        let mut waiter = Box::pin(profiles.prepare_service_maintenance(request, &store));
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(waiter);
        let _finished = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            profiles.maintenance_command.lock(),
        )
        .await
        .unwrap();
        let preparation = profiles.service_maintenance_preparation().unwrap();
        assert!(preparation.ready());
        assert_eq!(
            preparation.operation(),
            zenclash_core::ServiceOperation::Uninstall
        );
        assert!(!store.root().exists());
    }

    #[test]
    fn accepted_local_recovery_retires_old_service_confirmation_without_losing_newer_results() {
        let mut state = ProfileRecoveryState {
            runtime_result_version: Some(7),
            service_tun_pending: Some(7),
            service_tun_warning: Some("old service commit uncertainty".into()),
            ..ProfileRecoveryState::default()
        };
        state.accept_local_recovery(8);
        assert_eq!(state.runtime_result_version, Some(8));
        assert!(state.pending_finalization().is_none());
        assert!(state.service_tun_warning.is_none());
        state.runtime_result_version = Some(9);
        state.service_tun_pending = Some(9);
        state.accept_local_recovery(8);
        assert_eq!(state.pending_finalization(), Some(9));
    }

    fn committed_fixture() -> (
        PathBuf,
        ProfileStore,
        ProfileService,
        Arc<ProfileApplyOutcome>,
    ) {
        let root = std::env::temp_dir().join(format!(
            "zenclash-committed-confirmation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let previous = store.import_local(&source).unwrap();
        store.activate(&previous.id).unwrap();
        std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let profile = store.import_local(&source).unwrap();
        let path = store.activate(&profile.id).unwrap();
        let client =
            MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap();
        let session =
            CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path.clone()), Vec::new())
                .unwrap();
        let outcome = Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
            source_version: (&profile).into(),
            profile,
            path,
            cause: zenclash_core::ProfileApplicationError::Task("commit reply lost".into()),
            runtime_version: session.generation(),
        });
        (root, store, ProfileService::new(session, None), outcome)
    }

    #[test]
    fn directory_service_facts_keep_same_generation_warning_and_durable_receipt() {
        let (root, _, service, outcome) = committed_fixture();
        let generation = service.session.generation();
        service
            .finish_apply(outcome, ChangeContext::Selection)
            .unwrap();
        service.record_profile_service_facts(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::HotReloaded,
                generation,
            },
            true,
            Some("capture readback failed"),
        );
        let state = service.failure.lock();
        assert_eq!(
            state.service_tun_warning.as_deref(),
            Some("capture readback failed")
        );
        assert_eq!(state.service_tun_pending, Some(generation));
        assert!(matches!(
            state.unresolved.as_deref(),
            Some(ProfileApplyOutcome::CommittedButRuntimeUnknown { .. })
        ));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_service_facts_cannot_overwrite_a_newer_runtime_result() {
        let (root, _, service, outcome) = committed_fixture();
        let generation = service.session.generation();
        service
            .finish_apply(outcome, ChangeContext::Selection)
            .unwrap();
        let mut state = service.failure.lock();
        state.runtime_result_version = Some(generation + 1);
        state.service_tun_warning = Some("new warning".into());
        drop(state);
        service.record_profile_service_facts(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::HotReloaded,
                generation,
            },
            true,
            Some("old warning"),
        );
        assert_eq!(
            service.failure.lock().service_tun_warning.as_deref(),
            Some("new warning")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_source_returns_a_saved_receipt_and_separate_confirmation() {
        let (root, store, service, outcome) = committed_fixture();
        let receipt = service
            .finish_apply(outcome, ChangeContext::Selection)
            .unwrap();
        assert_eq!(
            receipt.runtime_version(),
            Some(service.session().generation())
        );
        assert!(receipt.warning().is_some());
        assert_eq!(service.pending_finalization(), receipt.runtime_version());
        assert!(
            service.latest_recovery().is_none(),
            "durable data must not offer old-source rollback"
        );
        assert_eq!(
            service.committed_profile().profile_path.as_deref(),
            Some(receipt.path())
        );
        assert_eq!(
            store.active_path().unwrap().as_deref(),
            Some(receipt.path())
        );
        assert!(
            std::fs::read_to_string(receipt.path())
                .unwrap()
                .contains("7891")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_service_confirmation_keeps_the_saved_source_and_pending_action() {
        let (root, store, service, outcome) = committed_fixture();
        let receipt = service
            .finish_apply(outcome, ChangeContext::Editor)
            .unwrap();
        let version = receipt.runtime_version().unwrap();
        let before = store.load().unwrap();
        let payload = std::fs::read(receipt.path()).unwrap();
        // A Direct client cannot confirm a Service commit. No activation or reload is allowed.
        assert!(service.confirm_service_runtime(version).await.is_err());
        assert_eq!(service.pending_finalization(), Some(version));
        assert_eq!(store.load().unwrap().active, before.active);
        assert_eq!(std::fs::read(receipt.path()).unwrap(), payload);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_service_tun_uncertainty_keeps_confirmation_without_source_rollback() {
        let (root, store, service, _) = committed_fixture();
        let path = store.active_path().unwrap().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let generation = service.session.generation();
        service.record_service_tun(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Restarted,
                generation,
            },
            true,
            None,
        );
        assert_eq!(service.pending_finalization(), Some(generation));
        assert!(service.latest_recovery().is_none());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn startup_saved_pending_receipt_is_confirmed_through_the_same_owner() {
        let (root, _, service, _) = committed_fixture();
        let generation = service.session.generation();
        let saved = CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::Restarted,
            generation,
        };
        service.record_initialization_facts(Some(&saved), true);
        assert_eq!(service.pending_finalization(), Some(generation));
        // Direct is deliberately not a native Service fixture: confirmation must
        // reject it rather than pretend this UI record can confirm Commit alone.
        assert!(service.confirm_service_runtime(generation).await.is_err());
        assert_eq!(service.pending_finalization(), Some(generation));
        assert!(service.latest_recovery().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_unsaved_or_stale_initialization_cannot_publish_a_receipt() {
        let (root, _, service, _) = committed_fixture();
        service.record_initialization_facts(None, true);
        assert!(service.pending_finalization().is_none());
        let saved = CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::Restarted,
            generation: service.session.generation() + 1,
        };
        service.record_initialization_facts(Some(&saved), true);
        assert!(service.pending_finalization().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn saved_capture_readback_failure_is_not_a_commit_to_confirm() {
        let (root, store, service, _) = committed_fixture();
        let path = store.active_path().unwrap().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let generation = service.session.generation();
        let warning = "capture readback failed after successful commit";
        service.record_service_tun(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Patched,
                generation,
            },
            false,
            Some(warning),
        );
        assert!(service.pending_finalization().is_none());
        assert!(service.confirm_service_runtime(generation).await.is_err());
        assert_eq!(service.service_tun_warning().as_deref(), Some(warning));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_confirmation_does_not_clear_independent_cleanup_warning() {
        let (root, _, service, _) = committed_fixture();
        let generation = service.session.generation();
        let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
        service.record_service_tun(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Patched,
                generation,
            },
            true,
            Some(&warning),
        );
        assert!(service.failure.lock().confirm_pending(generation));
        assert!(service.pending_finalization().is_none());
        assert_eq!(service.service_tun_warning(), Some(warning));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn late_service_tun_receipt_cannot_clear_a_new_binding_pending_commit() {
        let (root, _, service, outcome) = committed_fixture();
        let old_generation = service.session.generation();
        service
            .session
            .switch_to_direct(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:2", ""))
            .await
            .unwrap();
        let ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile,
            path,
            source_version,
            ..
        } = outcome.as_ref()
        else {
            unreachable!()
        };
        let current_generation = service.session.generation();
        service.record_outcome(&Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile: profile.clone(),
            path: path.clone(),
            source_version: source_version.clone(),
            cause: zenclash_core::ProfileApplicationError::Task("new commit reply lost".into()),
            runtime_version: current_generation,
        }));
        service.record_service_tun(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Restarted,
                generation: old_generation,
            },
            true,
            None,
        );
        assert_eq!(service.pending_finalization(), Some(current_generation));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_successful_mode_change_does_not_make_the_saved_commit_unconfirmable() {
        let (root, _, service, outcome) = committed_fixture();
        let receipt = service
            .finish_apply(outcome, ChangeContext::Selection)
            .unwrap();
        let version = receipt.runtime_version().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        service
            .session()
            .switch_to_direct(zenclash_core::MihomoEndpoint::new(
                format!("http://{address}"),
                "",
            ))
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            for (method, mode) in [("GET", "rule"), ("PATCH", ""), ("GET", "global")] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let count = stream.read(&mut request).await.unwrap();
                assert!(request[..count].starts_with(format!("{method} /configs").as_bytes()));
                let response = if method == "PATCH" {
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                } else {
                    let body = format!("{{\"mixed-port\":7891,\"mode\":\"{mode}\"}}");
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        service
            .session()
            .set_mode(
                &ControlledConfigStore::new(root.join("controlled")),
                "global",
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert!(service.session().generation() > version);
        assert_eq!(service.pending_finalization(), Some(version));
        let failure = service.confirm_service_runtime(version).await.unwrap_err();
        assert_eq!(
            failure,
            zenclash_i18n::text_with(
                "profiles.recovery.confirm_failed",
                &[(
                    "error",
                    zenclash_core::MihomoError::StaleBinding.to_string()
                )]
            ),
            "confirmation must reach the client instead of rejecting an unrelated mode generation"
        );
        assert_eq!(service.pending_finalization(), Some(version));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn late_service_confirmation_cannot_clear_a_newer_pending_commit() {
        let (root, _, service, outcome) = committed_fixture();
        service.record_outcome(&outcome);
        let ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile,
            path,
            source_version,
            runtime_version,
            ..
        } = outcome.as_ref()
        else {
            unreachable!()
        };
        service.record_outcome(&Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile: profile.clone(),
            path: path.clone(),
            source_version: source_version.clone(),
            cause: zenclash_core::ProfileApplicationError::Task("second commit reply lost".into()),
            runtime_version: runtime_version + 1,
        }));
        let mut state = service.failure.lock();
        assert!(!state.confirm_pending(*runtime_version));
        drop(state);
        assert_eq!(service.pending_finalization(), Some(runtime_version + 1));
        assert!(service.failure.lock().confirm_pending(runtime_version + 1));
        assert!(service.pending_finalization().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn delayed_acceptance_does_not_clear_a_newer_unknown_runtime() {
        Box::pin(assert_delayed_runtime_publication(false, false)).await;
    }

    #[tokio::test]
    async fn delayed_unknown_does_not_replace_a_newer_accepted_runtime() {
        Box::pin(assert_delayed_runtime_publication(true, false)).await;
    }

    #[tokio::test]
    async fn reapplying_the_current_profile_clears_shared_runtime_uncertainty() {
        Box::pin(assert_delayed_runtime_publication(true, true)).await;
    }

    async fn assert_delayed_runtime_publication(unknown_first: bool, reapply_current: bool) {
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-result-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        let first_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let second_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ports = [
            first_port.local_addr().unwrap().port(),
            second_port.local_addr().unwrap().port(),
        ];
        drop((first_port, second_port));
        std::fs::write(
            &source,
            format!("mixed-port: {}\nrules: [MATCH,DIRECT]\n", ports[0]),
        )
        .unwrap();
        let first = store.import_local(&source).unwrap();
        let path = store.activate(&first.id).unwrap();
        std::fs::write(
            &source,
            format!("mixed-port: {}\nrules: [MATCH,REJECT]\n", ports[1]),
        )
        .unwrap();
        let second = store.import_local(&source).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for lose_response in [unknown_first, !unknown_first] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let count = stream.read(&mut request).await.unwrap();
                assert!(request[..count].starts_with(b"PUT /configs"));
                if !lose_response {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
                // Losing the response intentionally leaves runtime acceptance uncertain.
            }
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let session =
            CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path), Vec::new())
                .unwrap();
        let service = ProfileService::new(session.clone(), Some(overrides));
        // Execute a real first transition, then pause its service continuation at the
        // await boundary while another entry point completes and publishes its result.
        let delayed = Arc::new(
            ProfileApplication::new(store.clone(), controlled.clone(), session)
                .apply(ProfileChange::ActivateExisting {
                    id: first.id,
                    overrides: Vec::new(),
                })
                .await,
        );
        assert_eq!(
            matches!(delayed.as_ref(), ProfileApplyOutcome::RuntimeUnknown { .. }),
            unknown_first
        );
        let latest_failed = if reapply_current {
            assert!(
                service
                    .finish_apply(Arc::clone(&delayed), ChangeContext::Selection)
                    .is_err()
            );
            assert!(service.latest_recovery().is_some());
            let applied = service
                .reapply_with_overrides(controlled, Vec::new())
                .await
                .unwrap();
            assert!(applied.is_some());
            assert!(service.latest_recovery().is_none());
            false
        } else {
            service
                .activate(store, controlled, second.id.clone())
                .await
                .is_err()
        };
        server.await.unwrap();
        assert_eq!(latest_failed, !unknown_first);
        let _ = service.finish_apply(delayed, ChangeContext::Selection);
        if unknown_first {
            assert!(
                service.latest_recovery().is_none(),
                "a delayed unknown result replaced the newer accepted runtime"
            );
        } else {
            assert_eq!(
                service
                    .latest_recovery()
                    .map(|recovery| recovery.attempted.profile_id),
                Some(second.id),
                "a delayed accepted result cleared the newer unknown runtime"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shared_recovery_survives_a_preflight_rejection_until_a_profile_is_accepted() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-service-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
        let previous = store.import_local(&source).unwrap();
        let path = store.activate(&previous.id).unwrap();
        std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let candidate = store.import_local(&source).unwrap();
        let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PUT /configs?force=true "));
            drop(stream); // The request may have been accepted, but its response is lost.
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PUT /configs?force=true "));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"GET /configs"));
            let body = r#"{"mixed-port":7890,"mode":"rule"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"PATCH /configs"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = stream.read(&mut request).await.unwrap();
            assert!(request[..count].starts_with(b"GET /configs"));
            let body = r#"{"mixed-port":7890,"mode":"global"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let session =
            CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path), Vec::new())
                .unwrap();
        let service = ProfileService::new(session.clone(), Some(overrides));
        let other_entry_point = service.clone();
        let failure = service
            .activate(store.clone(), controlled.clone(), candidate.id.clone())
            .await
            .err()
            .unwrap();
        assert!(matches!(
            failure._outcome.as_deref(),
            Some(ProfileApplyOutcome::RuntimeUnknown { .. })
        ));
        let recovery = other_entry_point.latest_recovery().unwrap();
        assert_eq!(recovery.attempted.profile_id, candidate.id);
        assert_eq!(
            recovery.last_known_good.as_ref().unwrap().profile_id,
            previous.id
        );
        assert!(
            other_entry_point
                .activate(store.clone(), controlled.clone(), "missing".into())
                .await
                .is_err()
        );
        assert_eq!(service.latest_recovery(), Some(recovery));
        let accepted = Arc::new(
            ProfileApplication::new(store.clone(), controlled.clone(), session.clone())
                .apply(ProfileChange::ActivateExisting {
                    id: previous.id.clone(),
                    overrides: Vec::new(),
                })
                .await,
        );
        let ProfileApplyOutcome::Applied {
            runtime_version, ..
        } = accepted.as_ref()
        else {
            panic!("the recovery profile was not accepted");
        };
        let applied_version = *runtime_version;
        session.set_mode(&controlled, "global").await.unwrap();
        assert!(session.generation() > applied_version);
        let receipt = other_entry_point
            .finish_apply(accepted, ChangeContext::Selection)
            .unwrap();
        server.await.unwrap();
        assert_eq!(receipt.runtime_version(), Some(applied_version));
        assert!(service.latest_recovery().is_none());
        assert_eq!(store.load().unwrap().active, Some(previous.id));
        std::fs::remove_dir_all(root).unwrap();
    }
}
