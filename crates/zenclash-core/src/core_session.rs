//! Serialized, intent-oriented access to runtime-core transitions.

use std::{
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::RwLock;
use thiserror::Error;
use tokio::runtime::Handle;

use crate::{
    CaptureOutcome, ControlledConfigError, ControlledConfigStore, CoreKind, CoreUpdateError,
    MihomoClient, MihomoError, MihomoProcess, MihomoRelease, MihomoReleaseService,
    TrafficCaptureSession, VersionInfo,
    controlled_config::{
        RuntimeApplicationTransaction, RuntimeCandidateValidation, RuntimeMutationError,
        SavedRuntimeReceipt,
    },
    data_coordinator::DataWriteLease,
};

const CORE_READY_TIMEOUT: Duration = Duration::from_secs(20);
const CORE_SUPERVISOR_INTERVAL: Duration = Duration::from_millis(250);
const CORE_RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_CORE_RECOVERY_ATTEMPTS: u32 = 3;

mod automatic;
mod binding;
mod permissions;
pub(crate) mod run_state;
mod service_recovery;
mod service_startup;
pub(crate) mod service_tun;

pub use service_recovery::CoreLocalRecoveryOutcome;
pub use service_startup::CoreInitializationOutcome;

#[cfg(test)]
pub(crate) mod ownership_tests;

#[cfg(test)]
mod real_backup_recovery;

#[derive(Clone, Copy)]
struct CoreRecoveryPolicy {
    interval: Duration,
    retry_delay: Duration,
    ready_timeout: Duration,
    max_attempts: u32,
}

type CoreRecoveryHookFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

trait CoreRecoveryCapture: Send + Sync {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_>;
    fn reconcile(&self) -> CoreRecoveryHookFuture<'_>;
}

impl CoreRecoveryCapture for TrafficCaptureSession {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move {
            match self.release_owned().await {
                Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move {
            match self.reconcile().await {
                Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

impl Default for CoreRecoveryPolicy {
    fn default() -> Self {
        Self {
            interval: CORE_SUPERVISOR_INTERVAL,
            retry_delay: CORE_RECOVERY_RETRY_DELAY,
            ready_timeout: CORE_READY_TIMEOUT,
            max_attempts: MAX_CORE_RECOVERY_ATTEMPTS,
        }
    }
}

/// Effective-configuration change requested by a runtime caller.
#[derive(Clone, Debug)]
pub enum EffectiveConfigIntent {
    /// Merge and persist a JSON patch over the active source profile.
    Patch {
        /// Initial source when this session has no committed configuration yet.
        profile: PathBuf,
        /// Recursive JSON object patch.
        patch: serde_json::Value,
        /// Initial override chain when this session has no committed configuration yet.
        overrides: Vec<PathBuf>,
    },
    /// Apply a different profile without changing its source file.
    ActivateProfile {
        /// Candidate source profile.
        profile: PathBuf,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Reapply the profile committed when this intent enters the transition gate.
    ReapplyCurrent {
        /// Current ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
}

/// Maintenance transition requested for a managed runtime core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreMaintenanceIntent {
    /// Restart the owned child and wait for its controller.
    Restart,
    /// Stop the owned child until an explicit restart is requested.
    Stop,
}

/// Mechanism that successfully applied an effective configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreApplyKind {
    /// The controller accepted a complete hot reload.
    HotReloaded,
    /// The controller accepted a verified partial update without reloading listeners.
    Patched,
    /// A managed child restarted with the generated runtime cache.
    Restarted,
}

/// Successful configuration transition and its monotonically increasing generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreApplyOutcome {
    /// Mechanism used for the accepted change.
    pub kind: CoreApplyKind,
    /// Session generation after the transition.
    pub generation: u64,
}

/// Accepted managed-core installation and its session generation.
#[derive(Debug)]
pub struct CoreInstallOutcome {
    /// Version reported by the accepted controller.
    pub version: VersionInfo,
    /// Session generation after the executable transition.
    pub generation: u64,
    /// Cleanup failure after acceptance; the new executable remains active.
    pub cleanup_error: Option<String>,
}

/// In-memory committed source identity paired with the current runtime generation.
///
/// The source may remain recorded while its owned core is stopped. This snapshot
/// reports the last committed source, rather than inferring identity from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreCommittedProfileSnapshot {
    /// Source successfully committed to this runtime session, absent before first application.
    pub profile_path: Option<PathBuf>,
    /// Runtime generation paired atomically with this source identity.
    pub generation: u64,
}

/// Opaque exact runtime configuration captured before a whole-data restore.
///
/// The snapshot retains the committed source identity and override chain together
/// with the applied startup payload. Source files are never recomputed on recovery.
#[derive(Clone)]
pub struct CoreRestoreSnapshot {
    committed: CommittedConfig,
    payload: Option<String>,
    service_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
}

/// Admission for one backup's activation, runtime reconciliation and disk cleanup.
///
/// Keep this guard alive through the completion task. Shutdown waits for every clone
/// before taking the runtime transition and stopping an owned kernel.
#[derive(Clone)]
pub struct CoreBackupAdmission {
    inner: Arc<BackupAdmissionInner>,
}

struct BackupAdmissionInner {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
pub(crate) enum RuntimeRestoreAuthority {
    Ordinary,
    Backup(CoreBackupAdmission),
}

#[derive(Clone)]
struct PendingBackupRestore {
    snapshot: CoreRestoreSnapshot,
    store_root: PathBuf,
    generation: u64,
}

/// Point-in-time state exposed by [`CoreSession`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreSessionSnapshot {
    /// Concrete runtime core.
    pub kind: CoreKind,
    /// Whether ZenClash owns the core process.
    pub managed: bool,
    /// Observed managed liveness; absent for external or unavailable service observations.
    pub running: Option<bool>,
    /// Runtime version, including recovery after an accepted temporary configuration.
    pub generation: u64,
}

/// Managed-core lifecycle phase observed by the session supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLifecyclePhase {
    /// The managed child is running and no recovery is in progress.
    Stable,
    /// An unexpected exit is being recovered with a bounded retry policy.
    Recovering,
    /// Recovery attempts were exhausted and no child is running.
    Failed,
    /// An explicit shutdown has started; recovery is permanently suppressed.
    ShuttingDown,
    /// The managed child was explicitly stopped and reaped.
    Stopped,
    /// Link loss stopped the child; only network recovery or an explicit restart may resume it.
    NetworkSuspended,
    /// The controller belongs to an external process ZenClash cannot supervise.
    External,
    /// Managed liveness or a lifecycle acknowledgement cannot currently be confirmed.
    Unknown,
}

/// Point-in-time state of managed-core recovery and shutdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreLifecycleSnapshot {
    /// Current lifecycle phase.
    pub phase: CoreLifecyclePhase,
    /// An admitted explicit Stop or application shutdown forbids automatic restart.
    /// This intention remains true even when the observed phase is Unknown.
    pub stop_requested: bool,
    /// Recovery attempts made for the latest unexpected exit.
    pub recovery_attempts: u32,
    /// Exit status captured from the owned child handle.
    pub exit_reason: Option<String>,
    /// Last failed recovery operation.
    pub last_error: Option<String>,
}

impl CoreLifecycleSnapshot {
    fn new(managed: bool) -> Self {
        Self {
            phase: if managed {
                CoreLifecyclePhase::Stable
            } else {
                CoreLifecyclePhase::External
            },
            stop_requested: false,
            recovery_attempts: 0,
            exit_reason: None,
            last_error: None,
        }
    }
}

/// Errors returned by intent-oriented core transitions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreSessionError {
    /// Retirement of the previous core resources after handover is unconfirmed.
    #[error("{}", zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))]
    PreviousCoreCleanupUnconfirmed,
    /// Effective configuration preparation, application, or persistence failed.
    #[error(transparent)]
    Config(#[from] ControlledConfigError),
    /// Managed release preparation, activation or recovery failed.
    #[error(transparent)]
    Update(#[from] CoreUpdateError),
    /// Managed process transition failed.
    #[error(transparent)]
    Process(#[from] MihomoError),
    /// An external core would require a process restart ZenClash does not own.
    #[error("外部 {core} 内核不支持需要重启的运行时变更")]
    ExternalRestartUnsupported {
        /// External runtime-core implementation.
        core: CoreKind,
    },
    /// The selected backend cannot install official Mihomo releases.
    #[error("{}", zenclash_i18n::text_with("core_page.errors.release_unsupported", &[("core", core.display_name().to_owned())]))]
    ReleaseUnsupported {
        /// Runtime core implementation that rejected this operation.
        core: CoreKind,
    },
    /// An operation raced with explicit application shutdown.
    #[error("内核会话正在停止，拒绝开始新的运行时操作")]
    ShuttingDown,
    /// A reapplication has no source configuration successfully committed to this session.
    #[error("{}", zenclash_i18n::text("overrides.notices.saved_unapplied"))]
    NoCommittedProfile,
}

/// Cloneable owner of serialized effective-config and lifecycle transitions.
#[derive(Clone)]
pub struct CoreSession {
    kind: CoreKind,
    client: MihomoClient,
    transition: Arc<tokio::sync::Mutex<CommittedConfig>>,
    backup_gate: Arc<tokio::sync::Mutex<()>>,
    capture_publication_gate: Arc<tokio::sync::Mutex<()>>,
    capture_intent_revision: Arc<AtomicU64>,
    pending_backup: Arc<RwLock<Option<PendingBackupRestore>>>,
    local_backup_recovery: Arc<RwLock<Option<LocalBackupRecovery>>>,
    committed_profile: Arc<RwLock<CommittedProfileCache>>,
    generation: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
    supervisor_started: Arc<AtomicBool>,
    network_suspended: Arc<AtomicBool>,
    offline_source: Option<Arc<OfflineCoreSource>>,
    lifecycle: Arc<RwLock<CoreLifecycleSnapshot>>,
    pub(crate) run_state: Arc<run_state::CoreRunState>,
}

// Offline identity can only be created with an application-owned disconnected client.
// A caller connecting to an external controller never receives this capability.
pub(crate) struct OfflineCoreSource {
    pub(crate) home: PathBuf,
    pub(crate) binary: Option<PathBuf>,
    binding: u64,
}

#[derive(Clone, Default)]
struct CommittedConfig {
    profile: Option<PathBuf>,
    overrides: Vec<PathBuf>,
}

#[derive(Clone)]
struct LocalBackupRecovery {
    binding: u64,
    active: Option<PathBuf>,
    // Unknown PUT outcomes reuse these exact files; neither possibly active slot is replaced.
    pending: Option<(Arc<crate::ServiceRuntimeBundle>, PathBuf, String)>,
}

struct CommittedProfileCache {
    config: CommittedConfig,
    generation: u64,
}

#[derive(Debug)]
pub(crate) enum CoreProfileStageError {
    Rejected(CoreSessionError),
    RuntimeUnknown {
        cause: CoreSessionError,
        runtime_version: u64,
    },
}

impl From<CoreSessionError> for CoreProfileStageError {
    fn from(error: CoreSessionError) -> Self {
        Self::Rejected(error)
    }
}

impl From<ControlledConfigError> for CoreProfileStageError {
    fn from(error: ControlledConfigError) -> Self {
        Self::Rejected(error.into())
    }
}

pub(crate) enum CoreProfileRollbackOutcome {
    Validated,
    Runtime {
        result: Result<(), CoreSessionError>,
        runtime_version: u64,
    },
}

#[must_use]
pub(crate) struct CoreProfileApplication {
    state: CoreProfileApplicationState,
    generation: Arc<AtomicU64>,
    committed_profile: Arc<RwLock<CommittedProfileCache>>,
    pending_backup: Arc<RwLock<Option<PendingBackupRestore>>>,
    client: MihomoClient,
    transition_guard: tokio::sync::OwnedMutexGuard<CommittedConfig>,
    overrides: Vec<PathBuf>,
    _write_lease: DataWriteLease,
    _run_attempt: Option<run_state::CoreRunAttempt>,
}

enum CoreProfileApplicationState {
    Runtime {
        transaction: Box<RuntimeApplicationTransaction>,
        kind: CoreApplyKind,
    },
    Validated(RuntimeCandidateValidation),
}

impl CoreProfileApplication {
    pub(crate) async fn commit(
        mut self,
        profile: PathBuf,
    ) -> Result<Option<CoreApplyOutcome>, (CoreSessionError, u64)> {
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, kind } => {
                let config = CommittedConfig {
                    profile: Some(profile),
                    overrides: self.overrides,
                };
                *self.transition_guard = config.clone();
                self.client.invalidate_connections();
                let generation = advance_profile_snapshot(
                    &self.generation,
                    &self.committed_profile,
                    &self.pending_backup,
                    Some(config),
                );
                // The catalog is already durable. Publish its identity even if finalization loses its ack.
                transaction
                    .commit()
                    .await
                    .map_err(|error| (CoreSessionError::Config(error), generation))?;
                Ok(Some(CoreApplyOutcome { kind, generation }))
            }
            CoreProfileApplicationState::Validated(_validation) => Ok(None),
        }
    }

    pub(crate) async fn rollback(self) -> CoreProfileRollbackOutcome {
        self.client.invalidate_connections();
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, .. } => {
                let result = transaction
                    .rollback()
                    .await
                    .map_err(CoreSessionError::Config);
                // Readers may have observed the accepted candidate before persistence failed.
                let runtime_version = advance_profile_snapshot(
                    &self.generation,
                    &self.committed_profile,
                    &self.pending_backup,
                    None,
                );
                CoreProfileRollbackOutcome::Runtime {
                    result,
                    runtime_version,
                }
            }
            CoreProfileApplicationState::Validated(_) => CoreProfileRollbackOutcome::Validated,
        }
    }
}

impl CoreSession {
    /// Opens an application-owned recovery session without a controller or kernel.
    /// Selected paths are retained for service health and maintenance only. This
    /// does not parse profiles, inspect executables, start a core or permit TUN.
    ///
    /// # Errors
    /// Rejects relative source paths or a failed disconnected client construction.
    pub fn open_offline(
        kind: CoreKind,
        home: PathBuf,
        binary: Option<PathBuf>,
    ) -> Result<Self, CoreSessionError> {
        if !home.is_absolute() || binary.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(
                MihomoError::InvalidInput("Offline source paths must be absolute".into()).into(),
            );
        }
        let client = MihomoClient::new(crate::MihomoEndpoint::new(
            "127.0.0.1:0",
            "zenclash-offline",
        ))?
        .with_core_kind(kind)?;
        let mut session = Self::open(kind, client)?;
        session.offline_source = Some(Arc::new(OfflineCoreSource {
            home,
            binary,
            binding: session.runtime_descriptor().binding_generation(),
        }));
        if let Some(source) = &session.offline_source {
            session.run_state.set_offline_source(source.clone());
        }
        Ok(session)
    }

    /// Reports whether the original application-owned disconnected binding remains active.
    /// An external controller, including one using the same endpoint, is never offline recovery.
    #[must_use]
    pub fn is_offline_recovery(&self) -> bool {
        self.offline_source().is_some()
    }

    pub(crate) fn offline_source(&self) -> Option<&OfflineCoreSource> {
        let source = self.offline_source.as_deref()?;
        let descriptor = self.runtime_descriptor();
        (descriptor.backend() == crate::CoreRuntimeBackend::Direct
            && descriptor.binding_generation() == source.binding)
            .then_some(source)
    }

    /// Opens a session whose controller binding is the sole runtime owner.
    ///
    /// # Errors
    /// Rejects a declared kind that differs from the binding's actual implementation.
    pub fn open(kind: CoreKind, client: MihomoClient) -> Result<Self, CoreSessionError> {
        Self::open_with_config(kind, client, None, Vec::new())
    }

    /// Opens a session with the source profile successfully applied at startup.
    ///
    /// The committed source identity is shared by queued mode and configuration
    /// changes and advances only after a successful profile transaction.
    ///
    /// # Errors
    /// Rejects a declared kind that differs from the binding's actual implementation.
    pub fn open_with_config(
        kind: CoreKind,
        client: MihomoClient,
        profile: Option<PathBuf>,
        overrides: Vec<PathBuf>,
    ) -> Result<Self, CoreSessionError> {
        let descriptor = client.runtime_descriptor();
        if descriptor.kind() != kind {
            return Err(MihomoError::InvalidInput(zenclash_i18n::text(
                "core_page.errors.runtime_kind_mismatch",
            ))
            .into());
        }
        let managed = descriptor.backend() != crate::CoreRuntimeBackend::Direct;
        client.mark_runtime_binding_used();
        Ok(Self {
            run_state: Arc::new(run_state::CoreRunState::new(&client)),
            kind,
            lifecycle: Arc::new(RwLock::new(CoreLifecycleSnapshot::new(managed))),
            client,
            committed_profile: Arc::new(RwLock::new(CommittedProfileCache {
                config: CommittedConfig {
                    profile: profile.clone(),
                    overrides: overrides.clone(),
                },
                generation: 0,
            })),
            transition: Arc::new(tokio::sync::Mutex::new(CommittedConfig {
                profile,
                overrides,
            })),
            backup_gate: Arc::new(tokio::sync::Mutex::new(())),
            capture_publication_gate: Arc::default(),
            capture_intent_revision: Arc::default(),
            pending_backup: Arc::new(RwLock::new(None)),
            local_backup_recovery: Arc::new(RwLock::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            supervisor_started: Arc::new(AtomicBool::new(false)),
            network_suspended: Arc::new(AtomicBool::new(false)),
            offline_source: None,
        })
    }

    /// Starts one bounded unexpected-exit supervisor for a managed core.
    ///
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor(&self, runtime: &Handle) -> bool {
        self.start_supervisor_with_policy(runtime, CoreRecoveryPolicy::default(), None)
    }

    /// Starts managed-core supervision with capture release and restoration hooks.
    ///
    /// On an unexpected exit, owned native capture is released before restart;
    /// after controller readiness succeeds, persistent intent is reconciled.
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor_with_capture(
        &self,
        runtime: &Handle,
        capture: TrafficCaptureSession,
    ) -> bool {
        self.start_supervisor_with_policy(
            runtime,
            CoreRecoveryPolicy::default(),
            Some(Arc::new(capture)),
        )
    }

    fn start_supervisor_with_policy(
        &self,
        runtime: &Handle,
        policy: CoreRecoveryPolicy,
        capture: Option<Arc<dyn CoreRecoveryCapture>>,
    ) -> bool {
        if !self.is_managed() || self.supervisor_started.swap(true, Ordering::AcqRel) {
            return false;
        }
        let session = self.clone();
        runtime.spawn(async move { supervise_managed_core(session, policy, capture).await });
        true
    }

    /// Returns the concrete runtime core.
    #[must_use]
    pub const fn kind(&self) -> CoreKind {
        self.kind
    }

    /// Returns the shared query client associated with the session.
    #[must_use]
    pub const fn client(&self) -> &MihomoClient {
        &self.client
    }

    /// Applies an effective-configuration intent through hot reload or managed restart.
    ///
    /// Explicit API rejection is returned directly. A managed core falls back
    /// to restart only after transport or response-decoding failure, where the
    /// hot-reload outcome is uncertain. External cores are never restarted.
    ///
    /// # Errors
    ///
    /// Returns preparation, validation, controller, persistence, or process errors.
    pub async fn apply(
        &self,
        store: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let _write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&_write_lease);
        let client = client.with_write_lease(&_write_lease)?;
        let active_profile = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let session = self.clone();
        // At most one admitted completion task owns this transition; shutdown waits for it.
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _write_lease = _write_lease;
            session
                .apply_admitted(store, client, active_profile, intent)
                .await
        })
        .await
        .map_err(|error| {
            self.mark_runtime_unknown();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn apply_admitted(
        &self,
        store: ControlledConfigStore,
        client: MihomoClient,
        mut active_profile: tokio::sync::OwnedMutexGuard<CommittedConfig>,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        if matches!(&intent, EffectiveConfigIntent::ReapplyCurrent { .. })
            && active_profile.profile.is_none()
        {
            return Err(CoreSessionError::NoCommittedProfile);
        }
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let (kind, profile, active_overrides, receipt) = match intent {
            EffectiveConfigIntent::Patch {
                profile,
                patch,
                overrides,
            } => {
                let (profile, overrides) = active_profile
                    .profile
                    .as_ref()
                    .map_or((profile, overrides), |profile| {
                        (profile.clone(), active_profile.overrides.clone())
                    });
                if client.service_client().is_some()
                    && crate::controlled_config::service_partial_patch(&patch)
                {
                    self.validate_backup_delta(&patch)?;
                }
                let (kind, receipt) = self
                    .apply_patch(&store, &client, profile.clone(), patch, overrides.clone())
                    .await?;
                (kind, profile, overrides, Some(receipt))
            }
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                let kind = self
                    .activate_profile(&store, &client, profile.clone(), overrides.clone())
                    .await?;
                (kind, profile, overrides, None)
            }
            EffectiveConfigIntent::ReapplyCurrent { overrides } => {
                let profile = active_profile
                    .profile
                    .clone()
                    .ok_or(CoreSessionError::NoCommittedProfile)?;
                let kind = self
                    .activate_profile(&store, &client, profile.clone(), overrides.clone())
                    .await?;
                (kind, profile, overrides, None)
            }
        };
        let committed = CommittedConfig {
            profile: Some(profile),
            overrides: active_overrides,
        };
        *active_profile = committed.clone();
        let generation = if let Some(receipt) = receipt {
            let generation = self.accept_saved_delta(Some(committed), receipt.delta.as_ref())?;
            receipt.confirmation?;
            generation
        } else {
            self.next_generation_with_config(Some(committed))
        };
        Ok(CoreApplyOutcome { kind, generation })
    }

    /// Changes the outbound mode on the currently committed profile.
    ///
    /// Resolves profile identity after entering the same transition gate used
    /// for profile application, maintenance and shutdown. An attached session
    /// without a source profile updates only the live controller.
    ///
    /// # Errors
    ///
    /// Returns controller, persistence or lifecycle errors. Failures after a
    /// runtime mutation attempt invalidate reads by advancing the generation;
    /// preflight rejection leaves the current generation unchanged.
    pub async fn set_mode(
        &self,
        store: &ControlledConfigStore,
        mode: &str,
    ) -> Result<u64, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let _write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&_write_lease);
        let client = client.with_write_lease(&_write_lease)?;
        let active_profile = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let session = self.clone();
        let mode = mode.to_owned();
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _write_lease = _write_lease;
            session
                .set_mode_admitted(store, client, active_profile, &mode)
                .await
        })
        .await
        .map_err(|error| {
            self.mark_runtime_unknown();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn set_mode_admitted(
        &self,
        store: ControlledConfigStore,
        client: MihomoClient,
        active_profile: tokio::sync::OwnedMutexGuard<CommittedConfig>,
        mode: &str,
    ) -> Result<u64, CoreSessionError> {
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let requested = serde_json::json!({"mode":mode.trim().to_ascii_lowercase()});
        self.validate_backup_delta(&requested)?;
        if let Some(profile) = active_profile.profile.as_ref() {
            let receipt = store
                .apply_mode_update_for_session(
                    &client,
                    profile,
                    mode,
                    active_profile.overrides.clone(),
                )
                .await
                .map_err(|error| self.runtime_mutation_error(error))?;
            let generation = self.accept_saved_delta(None, receipt.delta.as_ref())?;
            receipt.confirmation?;
            return Ok(generation);
        } else if let Err(error) = client.set_mode(mode).await {
            if !matches!(
                &error,
                MihomoError::InvalidInput(_) | MihomoError::StaleBinding
            ) {
                self.next_generation();
            }
            return Err(error.into());
        }
        self.accept_saved_delta(None, Some(&requested))
    }

    #[cfg(test)]
    pub(crate) async fn stage_profile_application(
        &self,
        store: &ControlledConfigStore,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
        apply_runtime: bool,
    ) -> Result<CoreProfileApplication, CoreProfileStageError> {
        self.stage_profile_application_expected(
            store,
            candidate,
            _previous,
            overrides,
            apply_runtime,
            None,
        )
        .await
    }

    pub(crate) async fn stage_profile_application_expected(
        &self,
        store: &ControlledConfigStore,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
        apply_runtime: bool,
        expected: Option<(u64, u64)>,
    ) -> Result<CoreProfileApplication, CoreProfileStageError> {
        let candidate = candidate.into();
        let client = self.client.pin_binding().map_err(CoreSessionError::from)?;
        let write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client
            .ensure_binding_current()
            .map_err(CoreSessionError::from)?;
        let store = store.with_write_lease(&write_lease);
        let client = client
            .with_write_lease(&write_lease)
            .map_err(CoreSessionError::from)?;
        let transition_guard = self.transition.clone().lock_owned().await;
        client
            .ensure_binding_current()
            .map_err(CoreSessionError::from)?;
        self.ensure_running_operations_allowed()?;
        if let Some((binding, generation)) = expected
            && (client.runtime_descriptor().binding_generation() != binding
                || self.generation() != generation)
        {
            return Err(CoreSessionError::from(MihomoError::StaleBinding).into());
        }
        let run_attempt = apply_runtime.then(|| self.begin_core_run_attempt());
        let active_overrides = overrides.clone();
        let state = if !apply_runtime {
            CoreProfileApplicationState::Validated(
                store
                    .stage_profile_validation(self.kind, &client, candidate, overrides)
                    .await?,
            )
        } else if !self.kind.capabilities().full_config_reload {
            CoreProfileApplicationState::Runtime {
                transaction: Box::new(
                    store
                        .stage_profile_restart(
                            self.local_process(&client)?,
                            candidate,
                            overrides,
                            Some(self.shutdown_requested.clone()),
                        )
                        .await
                        .map_err(|error| self.profile_stage_error(error))?,
                ),
                kind: CoreApplyKind::Restarted,
            }
        } else {
            let (transaction, kind) = store
                .stage_profile_reload_with_restart(
                    &client,
                    candidate,
                    overrides,
                    self.shutdown_requested.clone(),
                )
                .await
                .map_err(|error| self.profile_stage_error(error))?;
            CoreProfileApplicationState::Runtime {
                transaction: Box::new(transaction),
                kind,
            }
        };
        Ok(CoreProfileApplication {
            state,
            generation: self.generation.clone(),
            committed_profile: self.committed_profile.clone(),
            pending_backup: self.pending_backup.clone(),
            client,
            transition_guard,
            overrides: active_overrides,
            _write_lease: write_lease,
            _run_attempt: run_attempt,
        })
    }

    /// Performs a serialized managed-core maintenance transition.
    /// Cancellation while queued has no effects. Once admitted, completion retains
    /// the transaction guards even if the caller stops waiting.
    ///
    /// # Errors
    ///
    /// Returns an error for external cores or when restart/readiness fails.
    pub async fn maintain(&self, intent: CoreMaintenanceIntent) -> Result<u64, CoreSessionError> {
        self.maintain_with_timeout(intent, CORE_READY_TIMEOUT).await
    }

    /// Confirms service-kernel shutdown and exports its accepted resources and caches.
    /// Returns the published generation and an immutable ordinary-user recovery bundle.
    /// Once admitted, completion retains the transition even if its caller is cancelled.
    /// This does not release the service, perform administrator maintenance, or persist files.
    ///
    /// # Errors
    /// Rejects non-service owners, shutdown, unresolved revisions, and export failures.
    /// Failure retains the prior accepted bundle and the explicit stop intention.
    pub async fn export_service_runtime(
        &self,
    ) -> Result<(u64, Arc<crate::ServiceRuntimeBundle>), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let lease = self.acquire_process_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        let Some(crate::owned_core::OwnedCore::Service(runtime)) = client.owned_core() else {
            return Err(CoreSessionError::ReleaseUnsupported { core: self.kind });
        };
        self.ensure_not_shutting_down()?;
        self.lifecycle.write().stop_requested = true;
        self.network_suspended.store(false, Ordering::Release);
        let session = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let _run_attempt = session.begin_core_run_attempt();
            let _transition = transition;
            let _mutation = mutation;
            let result = runtime
                .stop_and_export()
                .await
                .map_err(CoreSessionError::Process);
            {
                let mut lifecycle = session.lifecycle.write();
                record_maintenance_outcome(&mut lifecycle, CoreMaintenanceIntent::Stop, &result);
                if session.is_shutting_down() {
                    lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                }
            }
            let generation = session.next_generation();
            Ok((generation, result?))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    async fn maintain_with_timeout(
        &self,
        intent: CoreMaintenanceIntent,
        timeout: Duration,
    ) -> Result<u64, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let lease = self.acquire_process_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        let owner = client
            .owned_core()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })?;
        let before = match &owner {
            crate::owned_core::OwnedCore::Local(process) => {
                let process = process.clone();
                tokio::task::spawn_blocking(move || process.try_snapshot())
                    .await
                    .map_err(|error| MihomoError::Process(error.to_string()))?
                    .ok()
            }
            crate::owned_core::OwnedCore::Service(_) => None,
        };
        self.ensure_not_shutting_down()?;
        let previous_network_suspended = self.network_suspended.swap(false, Ordering::AcqRel);
        let previous_lifecycle = {
            let mut lifecycle = self.lifecycle.write();
            let previous = (lifecycle.phase, lifecycle.stop_requested);
            lifecycle.stop_requested = intent == CoreMaintenanceIntent::Stop;
            previous
        };
        let local_observed = before.is_some();
        let session = self.clone();
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _transition = transition;
            let _mutation = mutation;
            let result = session
                .maintain_owned(&client, intent, timeout, &lease)
                .await;
            let changed = match owner {
                crate::owned_core::OwnedCore::Local(process) => {
                    let after = tokio::task::spawn_blocking(move || process.try_snapshot()).await;
                    match (before, after) {
                        (Some(before), Ok(Ok(after))) => before.pid != after.pid
                            || before.running != after.running || before.exit_reason != after.exit_reason,
                        _ => true,
                    }
                }
                crate::owned_core::OwnedCore::Service(_) => result.as_ref().err().is_some_and(|error| {
                    matches!(error, CoreSessionError::Process(error) if error.mutation_result_unknown())
                }),
            };
            {
                let mut lifecycle = session.lifecycle.write();
                if intent == CoreMaintenanceIntent::Restart
                    && result.is_err()
                    && local_observed
                    && !changed
                {
                    // Validation and TUN rejection cannot revoke an unchanged local owner.
                    lifecycle.phase = previous_lifecycle.0;
                    lifecycle.stop_requested = previous_lifecycle.1;
                    session
                        .network_suspended
                        .store(previous_network_suspended, Ordering::Release);
                } else {
                    record_maintenance_outcome(&mut lifecycle, intent, &result);
                }
                if session.is_shutting_down() {
                    lifecycle.stop_requested = true;
                    lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                }
            }
            if result.is_err() && changed {
                session.next_generation();
            }
            result?;
            Ok(session.next_generation())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    async fn maintain_owned(
        &self,
        client: &MihomoClient,
        intent: CoreMaintenanceIntent,
        timeout: Duration,
        lease: &DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => match intent {
                CoreMaintenanceIntent::Restart => {
                    process
                        .restart_and_wait_until_with_lease(
                            timeout,
                            Some(self.shutdown_requested.clone()),
                            lease,
                        )
                        .await?
                }
                CoreMaintenanceIntent::Stop => process.stop_async().await?,
            },
            Some(crate::owned_core::OwnedCore::Service(runtime)) => match intent {
                CoreMaintenanceIntent::Restart => {
                    runtime.restart_accepted(&self.shutdown_requested).await?
                }
                CoreMaintenanceIntent::Stop => runtime.stop_confirmed().await?,
            },
            None => return Err(CoreSessionError::ExternalRestartUnsupported { core: self.kind }),
        }
        Ok(())
    }

    /// Downloads a Mihomo release, then serializes executable activation and recovery.
    ///
    /// Dropping the caller does not abandon an activated file transaction. Application
    /// shutdown cancels preparation and prevents both candidate and recovery restarts.
    ///
    /// # Errors
    /// Returns unsupported-core, lifecycle, release verification or recovery errors.
    pub async fn install_release(
        &self,
        service: &MihomoReleaseService,
        release: &MihomoRelease,
    ) -> Result<CoreInstallOutcome, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        if self.kind != CoreKind::Mihomo {
            return Err(CoreSessionError::ReleaseUnsupported { core: self.kind });
        }
        let session = self.clone();
        let service = service.clone();
        let release = release.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let process = session.local_process(&client)?;
            let lease = session.acquire_process_write_lease(&client).await?;
            let client = client.with_write_lease(&lease)?;
            session.ensure_not_shutting_down()?;
            let prepared = service
                .prepare_until(
                    &release,
                    process.launch_config().binary.clone(),
                    Some(session.shutdown_requested.clone()),
                )
                .await?;
            let _transition = session.transition.lock().await;
            if let Err(error) = session.ensure_not_shutting_down().and_then(|()| {
                client
                    .ensure_binding_current()
                    .map_err(CoreSessionError::from)
            }) {
                tokio::task::spawn_blocking(move || drop(prepared))
                    .await
                    .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
                return Err(error);
            }
            let _mutation = client.lock_runtime_binding().await?;
            let _run_attempt = session.begin_core_run_attempt();
            let previous_pid = process.snapshot().pid;
            let previous_phase = session.lifecycle.read().phase;
            let result = service
                .install_prepared(
                    prepared,
                    process.clone(),
                    client,
                    session.shutdown_requested.clone(),
                    &lease,
                )
                .await;
            session.client.invalidate_connections();
            match result {
                Ok(crate::core_update::InstalledCore {
                    version,
                    cleanup_error,
                }) => {
                    let generation = session.next_generation();
                    session.network_suspended.store(false, Ordering::Release);
                    let mut lifecycle = session.lifecycle.write();
                    *lifecycle = CoreLifecycleSnapshot::new(true);
                    lifecycle.last_error = cleanup_error.clone();
                    if session.is_shutting_down() {
                        lifecycle.stop_requested = true;
                        lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                    }
                    Ok(CoreInstallOutcome {
                        version,
                        generation,
                        cleanup_error,
                    })
                }
                Err(error) => {
                    let snapshot = process.snapshot();
                    if snapshot.pid != previous_pid {
                        session.next_generation();
                    }
                    let running = snapshot.running;
                    let mut lifecycle = session.lifecycle.write();
                    lifecycle.phase = if session.is_shutting_down() {
                        CoreLifecyclePhase::ShuttingDown
                    } else if running {
                        CoreLifecyclePhase::Stable
                    } else if matches!(
                        previous_phase,
                        CoreLifecyclePhase::Stopped | CoreLifecyclePhase::NetworkSuspended
                    ) {
                        previous_phase
                    } else {
                        CoreLifecyclePhase::Failed
                    };
                    lifecycle.last_error = Some(error.to_string());
                    Err(error.into())
                }
            }
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    /// Updates GeoData through the same managed-directory and transition boundary.
    ///
    /// # Errors
    /// Returns lifecycle or controller failures.
    pub async fn update_geodata(&self) -> Result<(), CoreSessionError> {
        self.update_builtin_resource(true).await
    }

    /// Updates the external UI files through the same managed-directory boundary.
    ///
    /// # Errors
    /// Returns lifecycle or controller failures.
    pub async fn update_external_ui(&self) -> Result<(), CoreSessionError> {
        self.update_builtin_resource(false).await
    }

    async fn update_builtin_resource(&self, geodata: bool) -> Result<(), CoreSessionError> {
        let session = self.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let lease = session.acquire_process_write_lease(&client).await?;
            let client = client.with_write_lease(&lease)?;
            let _transition = session.transition.lock().await;
            client.ensure_binding_current()?;
            session.ensure_running_operations_allowed()?;
            if geodata {
                client.update_geodata().await?;
            } else {
                client.update_external_ui().await?;
            }
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) fn write_scopes(&self) -> Vec<PathBuf> {
        self.write_scopes_for_client(&self.client)
    }

    fn write_scopes_for_client(&self, client: &MihomoClient) -> Vec<PathBuf> {
        client.write_scopes()
    }

    async fn acquire_process_write_lease(
        &self,
        client: &MihomoClient,
    ) -> Result<DataWriteLease, CoreSessionError> {
        let lease = DataWriteLease::shared_async(self.write_scopes_for_client(client))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        client.ensure_binding_current()?;
        Ok(lease)
    }

    /// Stops the managed child, or detaches from an external controller.
    ///
    /// # Errors
    ///
    /// Returns an error when an owned child cannot be stopped and reaped.
    pub async fn shutdown(&self) -> Result<(), CoreSessionError> {
        self.request_shutdown();
        let session = self.clone();
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _backup = session.backup_gate.clone().lock_owned().await;
            let _capture = session.capture_publication_gate().lock_owned().await;
            let _transition = session.transition.clone().lock_owned().await;
            let client = session.client.pin_binding()?;
            let _mutation = client.lock_runtime_binding().await?;
            session.lifecycle.write().phase = CoreLifecyclePhase::ShuttingDown;
            let result = match client.owned_core() {
                Some(crate::owned_core::OwnedCore::Local(process)) => {
                    process.stop_async().await.map_err(CoreSessionError::from)
                }
                Some(crate::owned_core::OwnedCore::Service(runtime)) => runtime
                    .release_owned()
                    .await
                    .map_err(CoreSessionError::from),
                None => Ok(()),
            };
            session.next_generation();
            session.lifecycle.write().phase = if result.is_ok() {
                CoreLifecyclePhase::Stopped
            } else {
                CoreLifecyclePhase::Unknown
            };
            result
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    /// Returns managed recovery attempts, exit reason, and terminal phase.
    #[must_use]
    pub fn lifecycle_snapshot(&self) -> CoreLifecycleSnapshot {
        self.lifecycle.read().clone()
    }

    /// Returns process ownership without waiting for a lifecycle transition.
    #[must_use]
    pub fn is_managed(&self) -> bool {
        self.runtime_descriptor().backend() != crate::CoreRuntimeBackend::Direct
    }

    /// Returns the runtime version without inspecting the child.
    ///
    /// Recovery after an accepted temporary configuration also advances this version,
    /// so reads of that temporary state cannot remain current after rollback.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.committed_profile.read().generation
    }

    /// Reads the last committed runtime source and its paired generation.
    ///
    /// This only copies prepared in-memory state under a short lock. It never
    /// inspects files, the child process, or the asynchronous transition gate.
    #[must_use]
    pub fn committed_profile_snapshot(&self) -> CoreCommittedProfileSnapshot {
        let cached = self.committed_profile.read();
        CoreCommittedProfileSnapshot {
            profile_path: cached.config.profile.clone(),
            generation: cached.generation,
        }
    }

    // The caller has already reserved all session/data scopes exclusively, before swapping files.
    pub(crate) fn capture_restore_snapshot(
        &self,
        store: &ControlledConfigStore,
    ) -> Result<CoreRestoreSnapshot, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let committed = self.committed_profile.read().config.clone();
        let payload = store.cached_runtime_payload()?;
        let service_bundle = self.client.service_runtime_snapshot()?;
        Ok(CoreRestoreSnapshot {
            committed,
            payload,
            service_bundle,
        })
    }

    /// Reapplies the exact runtime payload captured before a data restore.
    ///
    /// Resolve this snapshot under the restore's exclusive write authority and
    /// pass its authorized controlled store. Source files and override documents
    /// may have changed; they are never read to recreate the previous runtime.
    ///
    /// # Errors
    ///
    /// Returns a missing-snapshot, validation, controller, process or exit error.
    pub async fn restore_snapshot(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        self.restore_snapshot_owned(store, snapshot, None, None)
            .await
    }

    /// Reserves one backup completion before its blocking activation starts.
    ///
    /// # Errors
    /// Returns [`CoreSessionError::ShuttingDown`] when shutdown has started.
    pub async fn begin_backup_restore(&self) -> Result<CoreBackupAdmission, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let guard = self.backup_gate.clone().lock_owned().await;
        self.ensure_not_shutting_down()?;
        Ok(CoreBackupAdmission {
            inner: Arc::new(BackupAdmissionInner {
                _guard: guard,
                gate: self.backup_gate.clone(),
            }),
        })
    }

    /// Restores an explicitly captured backup snapshot, completing admitted cleanup during shutdown.
    ///
    /// A saved service candidate is confirmed before the older immutable bundle is applied.
    /// Failed recovery retains the snapshot for [`Self::retry_backup_restore`].
    ///
    /// # Errors
    /// Returns an authority, confirmation, controller or snapshot error.
    pub async fn restore_backup_snapshot(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
        admission: &CoreBackupAdmission,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        self.restore_snapshot_owned(store, snapshot, Some(admission.clone()), None)
            .await
    }

    /// Returns the current failed backup recovery generation using only in-memory state.
    #[must_use]
    pub fn pending_backup_restore(&self) -> Option<u64> {
        self.pending_backup
            .read()
            .as_ref()
            .map(|pending| pending.generation)
    }

    /// Retries the retained exact backup snapshot without rereading provider or profile sources.
    ///
    /// # Errors
    /// Returns an exit, stale-generation, confirmation or runtime restoration error.
    pub async fn retry_backup_restore(&self) -> Result<CoreApplyOutcome, CoreSessionError> {
        let admission = self.begin_backup_restore().await?;
        let pending = self
            .pending_backup
            .read()
            .clone()
            .ok_or(CoreSessionError::NoCommittedProfile)?;
        let store = ControlledConfigStore::new(pending.store_root);
        self.restore_snapshot_owned(
            &store,
            &pending.snapshot,
            Some(admission),
            Some(pending.generation),
        )
        .await
    }

    async fn restore_snapshot_owned(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
        admission: Option<CoreBackupAdmission>,
        expected_generation: Option<u64>,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        if admission
            .as_ref()
            .is_some_and(|admission| !Arc::ptr_eq(&admission.inner.gate, &self.backup_gate))
        {
            return Err(CoreSessionError::Process(MihomoError::StaleTransport));
        }
        let client = self.client.pin_binding()?;
        let lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mut committed = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        if admission.is_none() {
            self.ensure_running_operations_allowed()?;
            if self.pending_backup.read().is_some() {
                return Err(CoreSessionError::Process(MihomoError::StaleTransport));
            }
        }
        if expected_generation.is_some_and(|generation| generation != self.generation()) {
            return Err(CoreSessionError::Process(MihomoError::StaleTransport));
        }
        if let Some(state) = self.local_backup_recovery.read().as_ref()
            && state.binding == client.runtime_descriptor().binding_generation()
            && let Some((held, _, _)) = &state.pending
            && !snapshot
                .service_bundle
                .as_ref()
                .is_some_and(|bundle| Arc::ptr_eq(held, bundle))
        {
            return Err(MihomoError::StaleBinding.into());
        }
        let snapshot = snapshot.clone();
        let session = self.clone();
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _lease = lease;
            let _admission = admission;
            let result = session
                .restore_snapshot_admitted(
                    &store,
                    &client,
                    &snapshot,
                    &mut committed,
                    _admission
                        .as_ref()
                        .map_or(RuntimeRestoreAuthority::Ordinary, |admission| {
                            RuntimeRestoreAuthority::Backup(admission.clone())
                        }),
                )
                .await;
            if _admission.is_some() {
                let generation = session.generation();
                *session.pending_backup.write() = if result.is_err() {
                    Some(PendingBackupRestore {
                        snapshot,
                        store_root: store.root().to_path_buf(),
                        generation,
                    })
                } else {
                    None
                };
            }
            result
        })
        .await
        .map_err(|error| {
            self.mark_runtime_unknown();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn restore_snapshot_admitted(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        snapshot: &CoreRestoreSnapshot,
        committed: &mut CommittedConfig,
        authority: RuntimeRestoreAuthority,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let mut payload = snapshot.payload.clone().ok_or_else(|| {
            CoreSessionError::Config(ControlledConfigError::Transaction(zenclash_i18n::text(
                "backup.errors.no_runtime_snapshot",
            )))
        })?;
        let mut local_recovery = None;
        let mut recovery_restart = None;
        if let (Some(crate::owned_core::OwnedCore::Local(local)), Some(bundle)) =
            (client.owned_core(), snapshot.service_bundle.as_ref())
        {
            let binding = client.runtime_descriptor().binding_generation();
            let launch = local.launch_config().config_file.clone();
            let root = store.root().join("local-runtime");
            let active = (launch == root.join("slot0/runtime.yaml")
                || launch == root.join("slot1/runtime.yaml"))
            .then_some(launch);
            if active.is_none() {
                if local.kind() != CoreKind::Mihomo
                    || local.launch_config().config_file != store.runtime_path()
                {
                    return Err(MihomoError::StaleBinding.into());
                }
                crate::verify_ordinary_local_executable(&local.launch_config().binary)?;
                recovery_restart = Some(local.clone());
            }
            let mut state = self
                .local_backup_recovery
                .read()
                .clone()
                .filter(|state| state.binding == binding)
                .unwrap_or(LocalBackupRecovery {
                    binding,
                    active,
                    pending: None,
                });
            let path = if let Some((held, path, prepared)) = &state.pending {
                if !Arc::ptr_eq(held, bundle) {
                    return Err(MihomoError::StaleBinding.into());
                }
                payload = prepared.clone();
                path.clone()
            } else {
                let (path, prepared) = bundle
                    .materialize_local_runtime_prepared(store, state.active.clone())
                    .await?;
                payload = prepared;
                state.pending = Some((bundle.clone(), path.clone(), payload.clone()));
                path
            };
            *self.local_backup_recovery.write() = Some(state);
            local.set_recovery_asset_root(Some(root));
            local_recovery = Some(path);
        }
        let process = if recovery_restart.is_some() {
            recovery_restart
        } else if self.kind.capabilities().full_config_reload {
            None
        } else {
            Some(self.local_process(client)?)
        };
        let kind = if process.is_some() {
            CoreApplyKind::Restarted
        } else {
            CoreApplyKind::HotReloaded
        };
        let result = store
            .restore_exact_runtime_payload(
                client,
                process,
                payload,
                snapshot.service_bundle.clone(),
                authority,
                self.shutdown_requested.clone(),
            )
            .await;
        if let Err(error) = result {
            if local_recovery.is_some()
                && !error.attempted
                && let Some(state) = self.local_backup_recovery.write().as_mut()
            {
                state.pending = None;
            }
            return Err(self.runtime_mutation_error(error));
        }
        if let Some(path) = local_recovery
            && let Some(state) = self.local_backup_recovery.write().as_mut()
        {
            state.active = Some(path);
            state.pending = None;
        }
        *committed = snapshot.committed.clone();
        Ok(CoreApplyOutcome {
            kind,
            generation: self.next_generation_with_config(Some(snapshot.committed.clone())),
        })
    }

    /// Captures ownership, running state, and the current runtime generation.
    /// Run off the UI thread because process inspection can wait for a transition.
    #[must_use]
    pub fn snapshot(&self) -> CoreSessionSnapshot {
        let Ok(client) = self.client.pin_binding() else {
            let descriptor = self.runtime_descriptor();
            return CoreSessionSnapshot {
                kind: descriptor.kind(),
                managed: descriptor.backend() != crate::CoreRuntimeBackend::Direct,
                running: None,
                generation: self.generation(),
            };
        };
        let owner = client.owned_core();
        let (kind, managed, mut running) = match owner {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                (process.kind(), true, Some(process.is_running()))
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => (
                CoreKind::Mihomo,
                true,
                runtime
                    .client
                    .snapshot()
                    .map(|status| status.is_active && status.core_pid.is_some()),
            ),
            None => (client.binding_snapshot_kind(), false, None),
        };
        if client.ensure_binding_current().is_err() {
            running = None;
        }
        CoreSessionSnapshot {
            kind,
            managed,
            running,
            generation: self.generation(),
        }
    }

    pub(crate) async fn observe_current<T>(
        &self,
        observation: impl FnOnce(CoreSessionSnapshot) -> T,
    ) -> T {
        let _transition = self.transition.lock().await;
        observation(self.snapshot())
    }

    pub(crate) fn managed_process_snapshot(&self) -> Option<crate::MihomoProcessSnapshot> {
        match self.client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => Some(process.snapshot()),
            _ => None,
        }
    }

    async fn apply_patch(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<(CoreApplyKind, SavedRuntimeReceipt), CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            let process = self.local_process(client)?;
            store
                .apply_json_update_with_restart_for_session(
                    process,
                    profile,
                    &patch,
                    overrides,
                    Some(self.shutdown_requested.clone()),
                )
                .await
                .map_err(|error| self.runtime_mutation_error(error))?;
            return Ok((
                CoreApplyKind::Restarted,
                SavedRuntimeReceipt {
                    delta: None,
                    confirmation: Ok(()),
                },
            ));
        }

        match store
            .apply_json_update_for_session(client, &profile, &patch, overrides.clone())
            .await
        {
            Ok(receipt) => Ok((
                if receipt.delta.is_some() {
                    CoreApplyKind::Patched
                } else {
                    CoreApplyKind::HotReloaded
                },
                receipt,
            )),
            Err(error)
                if should_restart_after_hot_reload(&error.cause)
                    && self.local_process(client).is_ok() =>
            {
                let hot_reload_attempted = error.attempted;
                store
                    .apply_json_update_with_restart_for_session(
                        self.local_process(client)?,
                        profile,
                        &patch,
                        overrides,
                        Some(self.shutdown_requested.clone()),
                    )
                    .await
                    .map_err(|mut error| {
                        error.attempted |= hot_reload_attempted;
                        self.runtime_mutation_error(error)
                    })?;
                Ok((
                    CoreApplyKind::Restarted,
                    SavedRuntimeReceipt {
                        delta: None,
                        confirmation: Ok(()),
                    },
                ))
            }
            Err(error) => Err(self.runtime_mutation_error(error)),
        }
    }

    async fn activate_profile(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            store
                .restart_with_overrides_for_session(
                    self.local_process(client)?,
                    profile,
                    overrides,
                    Some(self.shutdown_requested.clone()),
                )
                .await
                .map_err(|error| self.runtime_mutation_error(error))?;
            return Ok(CoreApplyKind::Restarted);
        }

        match store
            .reload_with_overrides_for_session(client, &profile, overrides.clone())
            .await
        {
            Ok(()) => Ok(CoreApplyKind::HotReloaded),
            Err(error)
                if should_restart_after_hot_reload(&error.cause)
                    && self.local_process(client).is_ok() =>
            {
                let hot_reload_attempted = error.attempted;
                store
                    .restart_with_overrides_for_session(
                        self.local_process(client)?,
                        profile,
                        overrides,
                        Some(self.shutdown_requested.clone()),
                    )
                    .await
                    .map_err(|mut error| {
                        error.attempted |= hot_reload_attempted;
                        self.runtime_mutation_error(error)
                    })?;
                Ok(CoreApplyKind::Restarted)
            }
            Err(error) => Err(self.runtime_mutation_error(error)),
        }
    }

    fn local_process(&self, client: &MihomoClient) -> Result<Arc<MihomoProcess>, CoreSessionError> {
        client.ensure_binding_current()?;
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => Ok(process),
            _ => Err(CoreSessionError::ExternalRestartUnsupported { core: self.kind }),
        }
    }

    fn validate_backup_delta(&self, delta: &serde_json::Value) -> Result<(), CoreSessionError> {
        // TUN preparation also includes the observed enable flag. Reserve its largest representation.
        let mut delta = delta.clone();
        if let Some(tun) = delta
            .get_mut("tun")
            .and_then(serde_json::Value::as_object_mut)
        {
            tun.entry("enable")
                .or_insert(serde_json::Value::Bool(false));
        }
        let pending = self.pending_backup.read().clone();
        if let Some(pending) = pending {
            let _ = snapshot_with_delta(&pending.snapshot, &delta)?;
        }
        Ok(())
    }

    fn accept_saved_delta(
        &self,
        config: Option<CommittedConfig>,
        delta: Option<&serde_json::Value>,
    ) -> Result<u64, CoreSessionError> {
        let Some(delta) = delta else {
            return Ok(self.next_generation_with_config(config));
        };
        // The explicit receipt proves persistence, even when Commit acknowledgement is unknown.
        let previous_pending = self.pending_backup.read().clone();
        let updated_snapshot = previous_pending
            .as_ref()
            .map(|pending| snapshot_with_delta(&pending.snapshot, delta))
            .transpose()?;
        // YAML processing happens outside locks read by UI snapshots. The transition
        // guard serializes the authoritative backup target and accepted generation.
        let mut committed = self.committed_profile.write();
        let mut pending = self.pending_backup.write();
        if let (Some(pending), Some(snapshot)) = (pending.as_mut(), updated_snapshot) {
            pending.snapshot = snapshot;
        }
        if let Some(config) = config {
            committed.config = config;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        committed.generation = generation;
        if let Some(pending) = pending.as_mut() {
            pending.generation = generation;
        }
        self.client.invalidate_connections();
        Ok(generation)
    }

    fn next_generation(&self) -> u64 {
        self.next_generation_with_config(None)
    }

    pub(crate) fn mark_runtime_unknown(&self) -> u64 {
        self.next_generation()
    }

    fn next_generation_with_config(&self, config: Option<CommittedConfig>) -> u64 {
        self.client.invalidate_connections();
        self.run_state
            .invalidate_changed_binding(self.runtime_descriptor().binding_generation());
        advance_profile_snapshot(
            &self.generation,
            &self.committed_profile,
            &self.pending_backup,
            config,
        )
    }

    fn runtime_mutation_error(&self, error: RuntimeMutationError) -> CoreSessionError {
        if error.attempted {
            self.next_generation();
        }
        error.cause.into()
    }

    fn profile_stage_error(&self, error: RuntimeMutationError) -> CoreProfileStageError {
        let cause = CoreSessionError::Config(error.cause);
        if error.attempted {
            CoreProfileStageError::RuntimeUnknown {
                cause,
                runtime_version: self.next_generation(),
            }
        } else {
            CoreProfileStageError::Rejected(cause)
        }
    }

    pub(crate) fn ensure_running_operations_allowed(&self) -> Result<(), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        if self.network_suspended.load(Ordering::Acquire) || self.lifecycle.read().stop_requested {
            return Err(CoreSessionError::Process(MihomoError::Process(
                zenclash_i18n::text("automatic.core_paused"),
            )));
        }
        Ok(())
    }

    fn ensure_not_shutting_down(&self) -> Result<(), CoreSessionError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            Err(CoreSessionError::ShuttingDown)
        } else {
            Ok(())
        }
    }
}

fn snapshot_with_delta(
    snapshot: &CoreRestoreSnapshot,
    delta: &serde_json::Value,
) -> Result<CoreRestoreSnapshot, CoreSessionError> {
    Ok(CoreRestoreSnapshot {
        committed: snapshot.committed.clone(),
        payload: snapshot
            .payload
            .as_deref()
            .map(|payload| crate::controlled_config::merge_held_delta(payload, delta))
            .transpose()?,
        service_bundle: snapshot
            .service_bundle
            .as_ref()
            .map(|bundle| bundle.with_delta(delta).map(Arc::new))
            .transpose()?,
    })
}

fn advance_profile_snapshot(
    generation: &AtomicU64,
    committed_profile: &RwLock<CommittedProfileCache>,
    pending_backup: &RwLock<Option<PendingBackupRestore>>,
    config: Option<CommittedConfig>,
) -> u64 {
    let accepted_config = config.is_some();
    let mut snapshot = committed_profile.write();
    if let Some(config) = config {
        snapshot.config = config;
    }
    let generation = generation.fetch_add(1, Ordering::AcqRel) + 1;
    snapshot.generation = generation;
    let mut pending = pending_backup.write();
    if accepted_config {
        // A durably accepted full runtime supersedes the old backup recovery target.
        *pending = None;
    } else if let Some(pending) = pending.as_mut() {
        // Failed/uncertain attempts invalidate reads, but cannot discard the only exact snapshot.
        pending.generation = generation;
    }
    generation
}

async fn supervise_managed_core(
    session: CoreSession,
    policy: CoreRecoveryPolicy,
    capture: Option<Arc<dyn CoreRecoveryCapture>>,
) {
    let mut owner_watch = zenclash_service_integration::runstate::OwnerWatch::new();
    let mut watched_binding = None;
    let mut service_capture_released = false;
    loop {
        if session.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        let client = match session.client.pin_binding() {
            Ok(client) => client,
            Err(_) => continue,
        };
        let observation = match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                tokio::task::spawn_blocking(move || {
                    let snapshot = process.snapshot();
                    (snapshot.running, snapshot.exit_reason)
                })
                .await
                .ok()
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => {
                use zenclash_service_integration::runstate::{OwnerStep, OwnerWatch};
                let binding = client.runtime_descriptor().binding_generation();
                if watched_binding != Some(binding) {
                    watched_binding = Some(binding);
                    owner_watch = OwnerWatch::new();
                    service_capture_released = false;
                }
                let lifecycle = session.lifecycle.read().clone();
                if lifecycle.stop_requested
                    || session.network_suspended.load(Ordering::Acquire)
                    || !runtime.observation_allowed()
                {
                    session.reconcile_run_state();
                    tokio::time::sleep(policy.interval).await;
                    continue;
                }
                let mut step = owner_watch.observe(runtime.client.owner_sample().await);
                if matches!(step, OwnerStep::VerifyTransport) {
                    let available = client.version().await.is_ok();
                    step = owner_watch.resolve_transport(available);
                }
                if client.ensure_binding_current().is_err() || !runtime.observation_allowed() {
                    continue;
                }
                match step {
                    OwnerStep::Continue => {
                        if let Some(status) = runtime.client.snapshot() {
                            let mut lifecycle = session.lifecycle.write();
                            if !lifecycle.stop_requested {
                                lifecycle.phase = if status.core_pid.is_some() {
                                    CoreLifecyclePhase::Stable
                                } else {
                                    CoreLifecyclePhase::Recovering
                                };
                                lifecycle.exit_reason = status.last_core_exit_reason;
                            }
                        }
                    }
                    OwnerStep::Recover(reason) => {
                        {
                            let mut lifecycle = session.lifecycle.write();
                            if !lifecycle.stop_requested {
                                lifecycle.phase = CoreLifecyclePhase::Unknown;
                                lifecycle.last_error =
                                    Some(format!("Service owner recovery required: {reason:?}"));
                            }
                        }
                        zenclash_service_integration::runstate::mark_service_unavailable_after_owner_loss(
                            &session.run_state.store,
                            reason,
                        );
                        session.reconcile_run_state();
                        let recovery =
                            zenclash_service_integration::runstate::owner_recovery_policy(
                                reason,
                                cfg!(target_os = "macos"),
                            );
                        if recovery.reset_system_proxy
                            && !service_capture_released
                            && let Some(capture) = capture.as_ref()
                        {
                            match capture.release_owned().await {
                                Ok(()) => service_capture_released = true,
                                Err(error) => {
                                    tracing::warn!(%error, "could not release capture after lost service ownership")
                                }
                            }
                        }
                    }
                    OwnerStep::VerifyTransport => {
                        unreachable!("transport check is resolved before dispatch")
                    }
                }
                session.reconcile_run_state();
                // Native Service owns crash recovery. A displaced app must never
                // run an automatic Start that takes ownership back from another session.
                tokio::time::sleep(policy.interval).await;
                continue;
            }
            None => {
                tokio::time::sleep(policy.interval).await;
                continue;
            }
        };
        if client.ensure_binding_current().is_err() {
            continue;
        }
        let decision = supervisor_observation(
            &mut session.lifecycle.write(),
            observation,
            session.network_suspended.load(Ordering::Acquire),
            session.shutdown_requested.load(Ordering::Acquire),
            session.kind,
            policy.max_attempts,
        );
        session.reconcile_run_state();
        let Some((new_exit, retry_exhausted)) = decision else {
            tokio::time::sleep(policy.interval).await;
            continue;
        };
        if retry_exhausted {
            tokio::time::sleep(policy.interval).await;
            continue;
        }
        if new_exit
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.release_owned().await
        {
            tracing::warn!(%error, "failed to release owned capture after managed-core exit");
        }

        if session.lifecycle.read().recovery_attempts > 0 {
            tokio::time::sleep(policy.retry_delay).await;
            if session.shutdown_requested.load(Ordering::Acquire) {
                return;
            }
        }

        let recovered = recover_managed_core(&session, policy).await;
        if recovered
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.reconcile().await
        {
            tracing::warn!(%error, "failed to reconcile capture after managed-core recovery");
        }
        if recovered || session.shutdown_requested.load(Ordering::Acquire) {
            tokio::time::sleep(policy.interval).await;
        }
    }
}

pub(crate) fn record_maintenance_outcome<T>(
    lifecycle: &mut CoreLifecycleSnapshot,
    intent: CoreMaintenanceIntent,
    result: &Result<T, CoreSessionError>,
) {
    lifecycle.stop_requested = intent == CoreMaintenanceIntent::Stop;
    lifecycle.phase = if result.is_err() {
        CoreLifecyclePhase::Unknown
    } else {
        match intent {
            CoreMaintenanceIntent::Restart => CoreLifecyclePhase::Stable,
            CoreMaintenanceIntent::Stop => CoreLifecyclePhase::Stopped,
        }
    };
}

pub(crate) fn supervisor_observation(
    lifecycle: &mut CoreLifecycleSnapshot,
    observation: Option<(bool, Option<String>)>,
    network_suspended: bool,
    shutdown: bool,
    kind: CoreKind,
    max_attempts: u32,
) -> Option<(bool, bool)> {
    // Observation cannot revoke an explicit lifecycle intention. Status failures
    // describe missing evidence, rather than permission to recover a stopped core.
    if shutdown
        || network_suspended
        || lifecycle.stop_requested
        || matches!(
            lifecycle.phase,
            CoreLifecyclePhase::Stopped
                | CoreLifecyclePhase::NetworkSuspended
                | CoreLifecyclePhase::ShuttingDown
        )
    {
        return None;
    }
    let Some((running, exit_reason)) = observation else {
        lifecycle.phase = CoreLifecyclePhase::Unknown;
        return None;
    };
    if running {
        if lifecycle.phase == CoreLifecyclePhase::Unknown {
            lifecycle.phase = CoreLifecyclePhase::Stable;
        }
        return None;
    }
    if network_suspended || lifecycle.phase == CoreLifecyclePhase::Stopped {
        return None;
    }
    let new_exit = lifecycle.phase == CoreLifecyclePhase::Stable;
    if new_exit {
        lifecycle.recovery_attempts = 0;
        lifecycle.exit_reason = exit_reason.or_else(|| Some(format!("{kind} 托管进程意外退出")));
        lifecycle.last_error = None;
    }
    let exhausted = lifecycle.recovery_attempts >= max_attempts;
    lifecycle.phase = if exhausted {
        CoreLifecyclePhase::Failed
    } else {
        CoreLifecyclePhase::Recovering
    };
    Some((new_exit, exhausted))
}

async fn recover_managed_core(session: &CoreSession, policy: CoreRecoveryPolicy) -> bool {
    let client = match session.client.pin_binding() {
        Ok(client) => client,
        Err(error) => {
            session.lifecycle.write().last_error = Some(error.to_string());
            return false;
        }
    };
    let lease = match session.acquire_process_write_lease(&client).await {
        Ok(lease) => lease,
        Err(error) => {
            session.lifecycle.write().last_error = Some(error.to_string());
            return false;
        }
    };
    let _transition = session.transition.lock().await;
    if let Err(error) = client.ensure_binding_current() {
        session.lifecycle.write().last_error = Some(error.to_string());
        return false;
    }
    if session.shutdown_requested.load(Ordering::Acquire)
        || session.network_suspended.load(Ordering::Acquire)
        || session.lifecycle.read().stop_requested
    {
        return false;
    }
    let _mutation = match client.lock_runtime_binding().await {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if client.owned_core().is_none() {
        return false;
    }
    let _run_attempt = session.begin_core_run_attempt();
    if session.snapshot().running == Some(true) {
        session.lifecycle.write().phase = CoreLifecyclePhase::Stable;
        return true;
    }
    let attempt = {
        let mut lifecycle = session.lifecycle.write();
        lifecycle.recovery_attempts = lifecycle.recovery_attempts.saturating_add(1);
        lifecycle.phase = CoreLifecyclePhase::Recovering;
        lifecycle.recovery_attempts
    };
    match session
        .maintain_owned(
            &client,
            CoreMaintenanceIntent::Restart,
            policy.ready_timeout,
            &lease,
        )
        .await
    {
        Ok(()) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.phase = CoreLifecyclePhase::Stable;
            lifecycle.last_error = None;
            session.next_generation();
            true
        }
        Err(error) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.last_error = Some(error.to_string());
            lifecycle.phase = if attempt >= policy.max_attempts {
                CoreLifecyclePhase::Failed
            } else {
                CoreLifecyclePhase::Recovering
            };
            false
        }
    }
}

pub(crate) fn should_restart_after_hot_reload(error: &ControlledConfigError) -> bool {
    matches!(error, ControlledConfigError::Profile(MihomoError::Http(_)))
}

#[cfg(test)]
pub(crate) async fn fixed_listener_ports_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static PORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    // These protocol fixtures assert exact 8011/8012 payloads. Concurrent probes
    // otherwise temporarily reserve each other's candidate ports.
    tokio::time::timeout(Duration::from_secs(60), PORTS.lock())
        .await
        .expect("fixed-port fixture did not release its resource guard")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::atomic::AtomicUsize;

    use crate::MihomoEndpoint;
    #[cfg(unix)]
    use crate::MihomoLaunchConfig;

    use super::*;

    #[tokio::test]
    async fn queued_mode_rejects_a_binding_switched_while_waiting_for_data() {
        queued_mode_rejects_changed_binding(false).await;
    }

    #[tokio::test]
    async fn queued_mode_rejects_a_binding_switched_while_waiting_for_transition() {
        queued_mode_rejects_changed_binding(true).await;
    }

    #[tokio::test]
    async fn admitted_mode_rejected_before_send_does_not_advance_generation() {
        use std::{future::Future, task::Context};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = lifecycle_test_root("admitted-mode-binding");
        std::fs::create_dir_all(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (received, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            stream.read_exact(&mut vec![0_u8; length]).await.unwrap();
            received.send(()).unwrap();
            released.await.unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
        let blocker = {
            let client = client.clone();
            tokio::spawn(async move { client.patch_configs(&serde_json::json!({})).await })
        };
        waiting.await.unwrap();
        let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut switch = Box::pin(client.switch_to_direct(MihomoEndpoint::new(
            format!("http://{}", replacement.local_addr().unwrap()),
            "",
        )));
        assert!(
            switch
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        let lease = DataWriteLease::shared([root.clone()]);
        let store = ControlledConfigStore::new(root.join("store")).with_write_lease(&lease);
        let pinned = client
            .pin_binding()
            .unwrap()
            .with_write_lease(&lease)
            .unwrap();
        let transition = session.transition.clone().lock_owned().await;
        let mut operation =
            Box::pin(session.set_mode_admitted(store, pinned, transition, "global"));
        assert!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        switch.await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), operation)
                .await
                .unwrap(),
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
        assert!(
            replacement
                .poll_accept(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        assert_eq!(session.generation(), 0);
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn queued_mode_rejects_changed_binding(wait_for_transition: bool) {
        use std::{future::Future, task::Context};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = lifecycle_test_root("mode-binding-admission");
        std::fs::create_dir_all(&root).unwrap();
        let store = ControlledConfigStore::new(root.join("store"));
        let client = MihomoClient::new(MihomoEndpoint::default())
            .unwrap()
            .with_config_validator(crate::CoreConfigValidator::new(
                CoreKind::Mihomo,
                root.join("unused-kernel"),
                root.join("home"),
            ))
            .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
        let lease = if wait_for_transition {
            DataWriteLease::shared([root.clone()])
        } else {
            DataWriteLease::exclusive([root.clone()])
        };
        let store = if wait_for_transition {
            store.with_write_lease(&lease)
        } else {
            store
        };
        let transition = session.transition.lock().await;
        let mut operation = Box::pin(session.set_mode(&store, "global"));
        assert!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_millis(300), listener.accept()).await
            else {
                return false;
            };
            let mut request = [0_u8; 4096];
            assert_ne!(stream.read(&mut request).await.unwrap(), 0);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            true
        });
        client
            .switch_to_direct(MihomoEndpoint::new(format!("http://{address}"), ""))
            .await
            .unwrap();
        drop(transition);
        drop(lease);
        assert!(matches!(
            operation.await,
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
        assert!(
            !server.await.unwrap(),
            "stale operation reached the replacement controller"
        );
        assert_eq!(session.generation(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn ordinary_public_snapshot_cannot_consume_pending_backup_without_admission() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("backup-ordinary-no-authority");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store.materialize(&source).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut payloads = Vec::new();
            for index in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(stream.read_u8().await.unwrap());
                }
                let headers = String::from_utf8(headers).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                assert!(headers.starts_with("PUT /configs"));
                payloads.push(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["payload"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                if index == 1 {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
            }
            payloads
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(source.clone()),
            vec![],
        )
        .unwrap();
        let snapshot = session.capture_restore_snapshot(&store).unwrap();
        let admission = session.begin_backup_restore().await.unwrap();
        assert!(
            session
                .restore_backup_snapshot(&store, &snapshot, &admission)
                .await
                .is_err()
        );
        assert_eq!(session.pending_backup_restore(), Some(session.generation()));
        drop(admission);
        let ordinary = session.restore_snapshot(&store, &snapshot).await;
        assert!(
            ordinary.is_err(),
            "public ordinary restore consumed protected pending backup recovery"
        );
        assert_eq!(session.pending_backup_restore(), Some(session.generation()));
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_file(store.runtime_path()).unwrap();
        let outcome = session.retry_backup_restore().await.unwrap();
        assert_eq!(outcome.generation, session.generation());
        assert!(session.pending_backup_restore().is_none());
        let payloads = server.await.unwrap();
        assert_eq!(payloads[0], payloads[1]);
        assert!(payloads[1].contains("8011"));
        session.next_generation();
        assert!(session.pending_backup_restore().is_none());
        session.shutdown().await.unwrap();
        assert!(matches!(
            session.begin_backup_restore().await,
            Err(CoreSessionError::ShuttingDown)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_backup_restore_retains_exact_snapshot_for_retry_without_sources() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("backup-retry-held-snapshot");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store.materialize(&source).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut payloads = Vec::new();
            for index in 0..6 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(stream.read_u8().await.unwrap());
                }
                let headers = String::from_utf8(headers).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                if matches!(index, 2 | 3) {
                    assert!(headers.starts_with("PATCH /configs"));
                } else if matches!(index, 1 | 4) {
                    assert!(headers.starts_with("GET /configs"));
                } else {
                    payloads.push(
                        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["payload"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    );
                }
                if matches!(index, 1 | 4) {
                    let body = r#"{"mode":"rule"}"#;
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                } else if matches!(index, 3 | 5) {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
            }
            payloads
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(source.clone()),
            vec![],
        )
        .unwrap();
        let snapshot = session.capture_restore_snapshot(&store).unwrap();
        let admission = session.begin_backup_restore().await.unwrap();
        assert!(
            session
                .restore_backup_snapshot(&store, &snapshot, &admission)
                .await
                .is_err()
        );
        assert_eq!(session.pending_backup_restore(), Some(session.generation()));
        drop(admission);
        assert!(session.set_mode(&store, "global").await.is_err());
        assert_eq!(
            session.pending_backup_restore(),
            Some(session.generation()),
            "failed mode request discarded the only exact backup snapshot"
        );
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_file(store.runtime_path()).unwrap();
        let outcome = session.retry_backup_restore().await.unwrap();
        assert_eq!(outcome.generation, session.generation());
        assert!(session.pending_backup_restore().is_none());
        let payloads = server.await.unwrap();
        assert_eq!(payloads[0], payloads[1]);
        assert!(payloads[1].contains("8011"));
        session.next_generation();
        assert!(session.pending_backup_restore().is_none());
        session.shutdown().await.unwrap();
        assert!(matches!(
            session.begin_backup_restore().await,
            Err(CoreSessionError::ShuttingDown)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn successful_partial_mode_is_merged_into_pending_backup_retry_target() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("backup-retry-saved-mode-delta");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store.materialize(&source).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut payloads = Vec::new();
            for index in 0..5 {
                let (mut stream, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(stream.read_u8().await.unwrap());
                }
                let headers = String::from_utf8(headers).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                if matches!(index, 0 | 4) {
                    assert!(headers.starts_with("PUT /configs"));
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    payloads.push(body["payload"].as_str().unwrap().to_owned());
                } else if index == 2 {
                    assert!(headers.starts_with("PATCH /configs"));
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                        serde_json::json!({"mode":"global"})
                    );
                } else {
                    assert!(headers.starts_with("GET /configs"));
                }
                if matches!(index, 1 | 3) {
                    let mode = if index == 1 { "rule" } else { "global" };
                    let body = format!(r#"{{"mode":"{mode}"}}"#);
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                } else if index != 0 {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
            }
            payloads
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(source.clone()),
            vec![],
        )
        .unwrap();
        let snapshot = session.capture_restore_snapshot(&store).unwrap();
        let admission = session.begin_backup_restore().await.unwrap();
        assert!(
            session
                .restore_backup_snapshot(&store, &snapshot, &admission)
                .await
                .is_err()
        );
        drop(admission);
        session.set_mode(&store, "global").await.unwrap();
        assert_eq!(session.pending_backup_restore(), Some(session.generation()));
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(store.runtime_path()).unwrap();
        session.retry_backup_restore().await.unwrap();
        let payloads = server.await.unwrap();
        let recovered: serde_yaml::Value = serde_yaml::from_str(&payloads[1]).unwrap();
        assert_eq!(
            recovered["mode"].as_str(),
            Some("global"),
            "backup retry must preserve a later durably accepted mode delta"
        );
        assert_eq!(recovered["rules"][0].as_str(), Some("MATCH,DIRECT"));
        assert!(session.pending_backup_restore().is_none());
        session.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn queued_mode_uses_the_profile_committed_by_the_preceding_transition() {
        queued_update_uses_committed_config(QueuedConfigUpdate::Mode).await;
    }

    #[tokio::test]
    async fn queued_patch_uses_the_profile_and_override_chain_committed_by_the_preceding_transition()
     {
        queued_update_uses_committed_config(QueuedConfigUpdate::Patch).await;
    }

    #[tokio::test]
    async fn queued_override_reapply_does_not_reactivate_the_previously_displayed_profile() {
        queued_update_uses_committed_config(QueuedConfigUpdate::Reapply).await;
    }

    #[tokio::test]
    async fn reapply_without_a_committed_profile_does_not_mutate_saved_configuration() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-reapply-uncommitted-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let yaml_override = root.join("saved.yaml");
        std::fs::write(&yaml_override, "allow-lan: true\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            session
                .apply(
                    &store,
                    EffectiveConfigIntent::ReapplyCurrent {
                        overrides: vec![yaml_override.clone()]
                    }
                )
                .await,
            Err(CoreSessionError::NoCommittedProfile)
        ));
        assert_eq!(
            std::fs::read_to_string(yaml_override).unwrap(),
            "allow-lan: true\n"
        );
        assert!(!store.runtime_path().exists());
        assert_eq!(session.generation(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn committed_profile_snapshot_does_not_read_disk_or_wait_for_an_active_transition() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-committed-snapshot-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.yaml");
        std::fs::write(&source, "mode: rule\n").unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::default()).unwrap(),
            Some(source.clone()),
            vec![],
        )
        .unwrap();
        std::fs::remove_file(&source).unwrap();
        let _active_transition = session.transition.lock().await;
        assert_eq!(
            session.committed_profile_snapshot(),
            CoreCommittedProfileSnapshot {
                profile_path: Some(source),
                generation: 0,
            }
        );
        std::fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn rolling_back_an_accepted_profile_invalidates_reads_of_the_temporary_runtime() {
        let _ports = fixed_listener_ports_guard().await;
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-rollback-generation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = root.join("a.yaml");
        let candidate = root.join("b.yaml");
        std::fs::write(&previous, "mixed-port: 8011\nmode: rule\n").unwrap();
        std::fs::write(&candidate, "mixed-port: 8012\nmode: rule\n").unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0_u8; 4096];
                    let length = stream.read(&mut bytes).unwrap();
                    assert_ne!(length, 0, "request closed before its body was received");
                    request.extend_from_slice(&bytes[..length]);
                    if let Some(header_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let body_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= header_end + 4 + body_length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                if index == 1 {
                    let body = r#"{"mixed-port":8012,"mode":"rule"}"#;
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                } else {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                }
            }
            requests
        });
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&previous, &[], CoreKind::Mihomo)
            .unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(previous.clone()),
            vec![],
        )
        .unwrap();
        let transaction = session
            .stage_profile_application(&store, candidate, Some(previous.clone()), vec![], true)
            .await
            .unwrap();
        let read_generation = session.generation();
        let observed_runtime = session.client.runtime_config().await.unwrap();
        match transaction.rollback().await {
            CoreProfileRollbackOutcome::Runtime { result, .. } => result.unwrap(),
            CoreProfileRollbackOutcome::Validated => panic!("runtime transaction was not applied"),
        }
        let requests = server.join().unwrap();
        let restored: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(requests[0].starts_with("PUT /configs?force=true "));
        assert!(requests[0].contains("8012"));
        assert!(requests[1].starts_with("GET /configs "));
        assert!(requests[2].starts_with("PUT /configs?force=true "));
        assert!(requests[2].contains("8011"));
        assert_eq!(observed_runtime.mixed_port, 8012);
        assert_eq!(restored["mixed-port"].as_u64(), Some(8011));
        assert_eq!(
            session.committed_profile_snapshot().profile_path,
            Some(previous)
        );
        assert!(
            session.generation() > read_generation,
            "a read of the accepted candidate remained current after runtime rollback"
        );
    }

    #[tokio::test]
    async fn restore_snapshot_replays_unnormalized_applied_bytes_after_sources_are_deleted() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("raw-runtime-snapshot");
        std::fs::create_dir_all(&root).unwrap();
        let previous = root.join("previous.yaml");
        let overlay = root.join("overlay.yaml");
        let candidate = root.join("candidate.yaml");
        std::fs::write(&previous, "mixed-port: 8011\nmode: rule\n").unwrap();
        std::fs::write(&overlay, "allow-lan: true\n").unwrap();
        std::fs::write(&candidate, "mixed-port: 8012\nmode: global\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides(&previous, std::slice::from_ref(&overlay))
            .unwrap();
        let applied_bytes = std::fs::read_to_string(store.runtime_path()).unwrap();
        assert!(!applied_bytes.contains("geox-url"));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut payloads = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0_u8; 4096];
                    let length = stream.read(&mut bytes).unwrap();
                    assert_ne!(length, 0);
                    request.extend_from_slice(&bytes[..length]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        assert!(headers.starts_with("PUT /configs?force=true "));
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            payloads.push(
                                serde_json::from_slice::<serde_json::Value>(&request[end + 4..])
                                    .unwrap()["payload"]
                                    .as_str()
                                    .unwrap()
                                    .to_owned(),
                            );
                            break;
                        }
                    }
                }
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            payloads
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(previous.clone()),
            vec![overlay.clone()],
        )
        .unwrap();
        let exclusive = DataWriteLease::exclusive(vec![root.clone()]);
        let authorized = store.with_write_lease(&exclusive);
        let snapshot = session.capture_restore_snapshot(&authorized).unwrap();
        assert_eq!(
            session
                .apply(
                    &authorized,
                    EffectiveConfigIntent::ActivateProfile {
                        profile: candidate,
                        overrides: vec![],
                    }
                )
                .await
                .unwrap()
                .generation,
            1
        );
        std::fs::remove_file(&previous).unwrap();
        std::fs::remove_file(&overlay).unwrap();
        assert_eq!(
            session
                .restore_snapshot(&authorized, &snapshot)
                .await
                .unwrap()
                .generation,
            2
        );
        let payloads = server.join().unwrap();
        assert!(payloads[0].contains("8012"));
        assert_eq!(
            payloads[1], applied_bytes,
            "restore normalized or regenerated the accepted payload"
        );
        assert_eq!(
            std::fs::read_to_string(store.runtime_path()).unwrap(),
            applied_bytes
        );
        assert_eq!(
            session.committed_profile_snapshot().profile_path,
            Some(previous)
        );
        assert_eq!(session.transition.lock().await.overrides, vec![overlay]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn mode_preflight_read_failure_preserves_runtime_version_and_startup_cache() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("mode-preflight-version");
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("source.yaml");
        std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
            .unwrap();
        let previous_cache = std::fs::read(store.runtime_path()).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut bytes = [0_u8; 4096];
                let length = stream.read(&mut bytes).unwrap();
                assert_ne!(length, 0);
                request.extend_from_slice(&bytes[..length]);
            }
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8(request).unwrap()
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(profile),
            vec![],
        )
        .unwrap();
        assert!(session.set_mode(&store, "global").await.is_err());
        assert!(server.join().unwrap().starts_with("GET /configs "));
        assert_eq!(
            session.generation(),
            0,
            "a failed preflight query must not invalidate runtime reads"
        );
        assert_eq!(std::fs::read(store.runtime_path()).unwrap(), previous_cache);
        assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn invalid_patch_does_not_invalidate_the_committed_runtime() {
        let _ports = fixed_listener_ports_guard().await;
        let root = lifecycle_test_root("invalid-patch-version");
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("source.yaml");
        std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
            .unwrap();
        let previous_cache = std::fs::read(store.runtime_path()).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
            Some(profile.clone()),
            vec![],
        )
        .unwrap();
        assert!(matches!(
            session
                .apply(
                    &store,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch: serde_json::json!([]),
                        overrides: vec![],
                    }
                )
                .await,
            Err(CoreSessionError::Config(ControlledConfigError::NotMapping))
        ));
        assert_eq!(session.generation(), 0);
        assert_eq!(std::fs::read(store.runtime_path()).unwrap(), previous_cache);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unreadable_applied_cache_is_rejected_before_runtime_application() {
        let root = lifecycle_test_root("invalid-cache-version");
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("source.yaml");
        std::fs::write(&profile, "mode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(store.runtime_path(), [0xff_u8]).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
            Some(profile.clone()),
            vec![],
        )
        .unwrap();
        assert!(matches!(
            session
                .stage_profile_application(&store, profile.clone(), Some(profile), vec![], true)
                .await,
            Err(CoreProfileStageError::Rejected(CoreSessionError::Config(
                ControlledConfigError::Io(_)
            )))
        ));
        assert_eq!(session.generation(), 0);
        assert_eq!(std::fs::read(store.runtime_path()).unwrap(), vec![0xff]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn direct_mode_rollback_invalidates_reads_of_the_temporary_runtime() {
        direct_rollback_invalidates_runtime_reads(true, false, false).await;
    }

    #[tokio::test]
    async fn direct_patch_rollback_invalidates_reads_of_the_temporary_runtime() {
        direct_rollback_invalidates_runtime_reads(false, false, false).await;
    }

    #[tokio::test]
    async fn direct_patch_rollback_restores_the_applied_cache_after_sources_and_overrides_change() {
        direct_rollback_invalidates_runtime_reads(false, true, false).await;
    }

    #[tokio::test]
    async fn direct_patch_without_an_applied_cache_reports_unknown_after_persistence_failure() {
        direct_rollback_invalidates_runtime_reads(false, false, true).await;
    }

    async fn direct_rollback_invalidates_runtime_reads(
        mode_update: bool,
        changed_sources: bool,
        missing_cache: bool,
    ) {
        let _ports = fixed_listener_ports_guard().await;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = lifecycle_test_root(if mode_update {
            "direct-mode-rollback"
        } else {
            "direct-patch-rollback"
        });
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("source.yaml");
        std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
        let overlay = root.join("override.yaml");
        std::fs::write(&overlay, "allow-lan: true\n").unwrap();
        let overrides = if changed_sources {
            vec![overlay.clone()]
        } else {
            vec![]
        };
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&profile, &overrides, CoreKind::Mihomo)
            .unwrap();
        let applied_payload = std::fs::read_to_string(store.runtime_path()).unwrap();
        if missing_cache {
            std::fs::remove_file(store.runtime_path()).unwrap();
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(tokio::sync::Mutex::new((8011_u16, "rule".to_owned())));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered = Arc::new(parking_lot::Mutex::new(Some(entered_tx)));
        let release = Arc::new(tokio::sync::Notify::new());
        let payloads = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let server_payloads = payloads.clone();
        let server_state = state.clone();
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            for _ in 0..if mode_update {
                6
            } else if missing_cache {
                2
            } else {
                3
            } {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let state = server_state.clone();
                let payloads = server_payloads.clone();
                let release = server_release.clone();
                let entered = entered.clone();
                workers.spawn(async move {
                    let mut request = Vec::new();
                    let header_end = loop {
                        let mut bytes = [0_u8; 4096];
                        let length = stream.read(&mut bytes).await.unwrap();
                        assert_ne!(length, 0);
                        request.extend_from_slice(&bytes[..length]);
                        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&request[..end]);
                            let length = headers.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                            }).unwrap_or(0);
                            if request.len() >= end + 4 + length { break end; }
                        }
                    };
                    let mutation = !request.starts_with(b"GET ");
                    let body = if mutation {
                        let patch: serde_json::Value = serde_json::from_slice(&request[header_end + 4..]).unwrap();
                        let mut current = state.lock().await;
                        if mode_update {
                            current.1 = patch["mode"].as_str().unwrap().to_owned();
                        } else {
                            payloads.lock().push(patch["payload"].as_str().unwrap().to_owned());
                            let payload: serde_yaml::Value = serde_yaml::from_str(patch["payload"].as_str().unwrap()).unwrap();
                            current.0 = payload["mixed-port"].as_u64().unwrap() as u16;
                        }
                        String::new()
                    } else {
                        let current = state.lock().await;
                        serde_json::json!({"mixed-port": current.0, "mode": current.1}).to_string()
                    };
                    if mutation {
                        let first = entered.lock().take();
                        if let Some(entered) = first {
                            entered.send(()).unwrap();
                            tokio::time::timeout(Duration::from_secs(5), release.notified()).await.unwrap();
                        }
                    }
                    let status = if mutation { "204 No Content" } else { "200 OK" };
                    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                });
            }
            while let Some(result) = workers.join_next().await {
                result.unwrap();
            }
        });
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
            Some(profile.clone()),
            overrides,
        )
        .unwrap();
        if changed_sources {
            std::fs::write(&profile, "mixed-port: 7998\nmode: rule\n").unwrap();
            std::fs::write(&overlay, "allow-lan: false\n").unwrap();
        }
        let mutation_session = session.clone();
        let mutation_store = store.clone();
        let mutation_profile = profile.clone();
        let mutation = tokio::spawn(async move {
            if mode_update {
                mutation_session
                    .set_mode(&mutation_store, "global")
                    .await
                    .map(|_| ())
            } else {
                mutation_session
                    .apply(
                        &mutation_store,
                        EffectiveConfigIntent::Patch {
                            profile: mutation_profile,
                            patch: serde_json::json!({"mixed-port":8012}),
                            overrides: vec![],
                        },
                    )
                    .await
                    .map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        let read_generation = session.generation();
        let temporary = session.client.runtime_config().await.unwrap();
        let concurrent = store
            .prepare_json_update(&profile, &serde_json::json!({"allow-lan":true}))
            .unwrap();
        store.commit(&concurrent).unwrap();
        release.notify_one();
        let result = mutation.await.unwrap();
        server.await.unwrap();
        assert!(
            matches!(
                result,
                Err(CoreSessionError::Config(
                    ControlledConfigError::Transaction(_)
                ))
            ),
            "{result:?}"
        );
        if missing_cache {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&zenclash_i18n::text("backup.errors.no_runtime_snapshot"))
            );
            assert!(!store.runtime_path().exists());
            assert_eq!(
                payloads.lock().len(),
                1,
                "missing applied cache must not trigger a guessed runtime restore"
            );
            assert_eq!(state.lock().await.0, 8012);
            assert!(session.generation() > read_generation);
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        let restored: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
        let runtime = state.lock().await.clone();
        std::fs::remove_dir_all(root).unwrap();
        if changed_sources {
            assert_eq!(
                payloads.lock().last(),
                Some(&applied_payload),
                "rollback rebuilt modified source or override files instead of restoring the applied payload"
            );
        }
        assert_eq!(runtime, (8011, "rule".to_owned()));
        assert_eq!(restored["mixed-port"].as_u64(), Some(8011));
        assert_eq!(restored["mode"].as_str(), Some("rule"));
        if mode_update {
            assert_eq!(temporary.mode, "global");
        } else {
            assert_eq!(temporary.mixed_port, 8012);
        }
        assert!(
            session.generation() > read_generation,
            "temporary runtime query remained current after direct rollback"
        );
    }

    #[derive(Clone, Copy)]
    enum QueuedConfigUpdate {
        Mode,
        Patch,
        Reapply,
    }

    async fn queued_update_uses_committed_config(update: QueuedConfigUpdate) {
        let _ports = fixed_listener_ports_guard().await;
        let complete_reload = !matches!(update, QueuedConfigUpdate::Mode);
        let root = std::env::temp_dir().join(format!(
            "zenclash-queued-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a.yaml");
        let b = root.join("b.yaml");
        let previous_override = root.join("previous-override.yaml");
        let next_override = root.join("next-override.yaml");
        std::fs::write(&a, "mixed-port: 8011\nrules: [MATCH,DIRECT]\n").unwrap();
        std::fs::write(&b, "mixed-port: 8012\nrules: [MATCH,DIRECT]\n").unwrap();
        std::fs::write(&previous_override, "allow-lan: false\n").unwrap();
        std::fs::write(&next_override, "allow-lan: true\n").unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let mut entered = Some(entered);
            let mut requests = Vec::new();
            for index in 0..if complete_reload { 2 } else { 4 } {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0_u8; 8_192];
                let length = stream.read(&mut bytes).unwrap();
                requests.push(String::from_utf8_lossy(&bytes[..length]).into_owned());
                if index == 0 {
                    entered.take().unwrap().send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if complete_reload || matches!(index, 0 | 2) {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                } else {
                    let mode = if index == 1 { "rule" } else { "global" };
                    let body = format!(r#"{{"mode":"{mode}"}}"#);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            }
            requests
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            Some(a.clone()),
            vec![previous_override.clone()],
        )
        .unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let activation = {
            let session = session.clone();
            let store = store.clone();
            tokio::spawn(async move {
                session
                    .apply(
                        &store,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: b,
                            overrides: vec![next_override],
                        },
                    )
                    .await
            })
        };
        waiting.await.unwrap();
        let mode = {
            let session = session.clone();
            let store = store.clone();
            tokio::spawn(async move {
                if matches!(update, QueuedConfigUpdate::Reapply) {
                    session
                        .apply(
                            &store,
                            EffectiveConfigIntent::ReapplyCurrent {
                                overrides: vec![previous_override],
                            },
                        )
                        .await
                        .map(|outcome| outcome.generation)
                } else if matches!(update, QueuedConfigUpdate::Patch) {
                    session
                        .apply(
                            &store,
                            EffectiveConfigIntent::Patch {
                                profile: a,
                                patch: serde_json::json!({"mode": "global"}),
                                overrides: vec![previous_override],
                            },
                        )
                        .await
                        .map(|outcome| outcome.generation)
                } else {
                    session.set_mode(&store, "global").await
                }
            })
        };
        release.send(()).unwrap();
        assert_eq!(activation.await.unwrap().unwrap().generation, 1);
        assert_eq!(mode.await.unwrap().unwrap(), 2);
        assert_eq!(
            session.committed_profile_snapshot(),
            CoreCommittedProfileSnapshot {
                profile_path: Some(root.join("b.yaml")),
                generation: 2,
            }
        );
        let runtime: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
        assert_eq!(runtime["mixed-port"].as_u64(), Some(8012));
        if !matches!(update, QueuedConfigUpdate::Reapply) {
            assert_eq!(runtime["mode"].as_str(), Some("global"));
        }
        assert_eq!(
            runtime["allow-lan"].as_bool(),
            Some(!matches!(update, QueuedConfigUpdate::Reapply))
        );
        let requests = server.join().unwrap();
        assert!(
            requests[if complete_reload { 1 } else { 2 }].starts_with(if complete_reload {
                "PUT /configs?force=true "
            } else {
                "PATCH /configs "
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn queued_mode_does_not_run_after_shutdown_is_requested() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let guard = session.transition.lock().await;
        let task = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .set_mode(&ControlledConfigStore::new(std::env::temp_dir()), "global")
                    .await
            })
        };
        session.request_shutdown();
        drop(guard);
        assert!(matches!(
            task.await.unwrap(),
            Err(CoreSessionError::ShuttingDown)
        ));
        assert_eq!(session.generation(), 0);
    }

    #[tokio::test]
    async fn accepted_profile_apply_reports_hot_reload_and_advances_generation() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
            request
        });
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-apply-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client =
            MihomoClient::new(crate::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();

        let outcome = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile,
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
        assert_eq!(outcome.generation, 1);
        assert_eq!(session.snapshot().generation, 1);
        assert!(request.starts_with("PUT /configs?force=true "));
        assert!(store.runtime_path().is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_meow_never_fakes_a_restart_capability() {
        let session = CoreSession::open(
            CoreKind::Meow,
            MihomoClient::new(crate::MihomoEndpoint::default())
                .unwrap()
                .with_core_kind(CoreKind::Meow)
                .unwrap(),
        )
        .unwrap();
        let store = ControlledConfigStore::new(std::env::temp_dir().join(format!(
            "zenclash-core-session-external-{}",
            std::process::id()
        )));

        let error = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: PathBuf::from("unused.yaml"),
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CoreSessionError::ExternalRestartUnsupported {
                core: CoreKind::Meow
            }
        ));
        assert_eq!(session.snapshot().generation, 0);
    }

    #[tokio::test]
    async fn uncertain_external_mihomo_apply_never_becomes_an_owned_restart() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            assert!(stream.read(&mut request).unwrap() > 0);
        });
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-external-uncertain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            session.apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile,
                    overrides: Vec::new(),
                },
            ),
        )
        .await
        .expect("external controller failure did not remain bounded")
        .unwrap_err();
        server.join().unwrap();

        assert!(matches!(
            error,
            CoreSessionError::Config(ControlledConfigError::Profile(MihomoError::Http(_)))
        ));
        let snapshot = session.snapshot();
        assert!(!snapshot.managed);
        assert!(
            snapshot.running.is_none(),
            "external process ownership is not inferred"
        );
        assert_eq!(
            snapshot.generation, 1,
            "the uncertain reload must invalidate previous runtime reads"
        );
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::External
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_config_rejection_is_not_a_restart_signal() {
        let error = ControlledConfigError::Profile(MihomoError::Api {
            status: 400,
            message: "invalid config".into(),
        });

        assert!(!should_restart_after_hot_reload(&error));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unexpected_exit_retries_are_bounded_and_end_in_visible_failure() {
        let root = lifecycle_test_root("bounded-recovery");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; printf crash >&2; exit 23",
                launches.display()
            ),
        );
        let process = spawn_lifecycle_process(&root, binary);
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_millis(50),
                max_attempts: 2,
            },
            None,
        ));
        assert!(!session.start_supervisor(&Handle::current()));

        tokio::time::timeout(Duration::from_secs(10), async {
            while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervisor did not reach bounded failure");
        let lifecycle = session.lifecycle_snapshot();
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();

        assert_eq!(lifecycle.recovery_attempts, 2);
        assert!(lifecycle.exit_reason.is_some());
        assert!(lifecycle.last_error.is_some());
        assert_eq!(launch_count, 3, "initial launch plus exactly two retries");
        assert!(!process.is_running());
        session.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manual_recovery_after_exhaustion_rearms_the_supervisor() {
        let root = lifecycle_test_root("rearmed-supervisor");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let endpoint = MihomoEndpoint::new(format!("http://{address}"), "");
        write_lifecycle_script(
            &binary,
            &format!("printf x >> '{}'; exit 23", launches.display()),
        );
        let process =
            spawn_lifecycle_process_with_endpoint(&root, binary.clone(), endpoint.clone());
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_millis(50),
                max_attempts: 1,
            },
            None,
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initial recovery did not exhaust its retry");

        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display()
            ),
        );
        let listener = TcpListener::bind(address).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"meta":true,"version":"test"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        assert_eq!(
            session
                .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(1))
                .await
                .unwrap(),
            1
        );
        server.join().unwrap();
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::Stable
        );

        process.stop_async().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let lifecycle = session.lifecycle_snapshot();
                if lifecycle.phase == CoreLifecyclePhase::Failed && lifecycle.recovery_attempts == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervisor was not rearmed after the manual recovery");
        assert!(
            (3..=4).contains(&std::fs::read_to_string(&launches).unwrap().len()),
            "the rearmed retry may be stopped before its script is scheduled"
        );
        session.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_cancels_a_restart_waiter_and_prevents_a_late_child() {
        let root = lifecycle_test_root("shutdown-cancel");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display()
            ),
        );
        let process = spawn_lifecycle_process(&root, binary);
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        let restarting = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(2))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_to_string(&launches).map_or(0, |value| value.len()) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("restart did not launch its candidate child");

        tokio::time::timeout(Duration::from_secs(1), session.shutdown())
            .await
            .expect("shutdown waited for the full readiness timeout")
            .unwrap();
        let restart_error = restarting.await.unwrap().unwrap_err().to_string();
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(restart_error.contains("停止请求"));
        assert_eq!(launch_count, 2, "shutdown must not permit a later restart");
        assert!(!process.is_running());
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::Stopped
        );
        assert!(matches!(
            session.maintain(CoreMaintenanceIntent::Restart).await,
            Err(CoreSessionError::ShuttingDown)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_crash_recovery_releases_then_reconciles_capture_once() {
        let root = lifecycle_test_root("capture-hooks");
        let launches = root.join("launches");
        let crashed = root.join("crashed");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; if [ ! -f '{}' ]; then touch '{}'; exit 23; fi; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display(),
                crashed.display(),
                crashed.display(),
            ),
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"meta":true,"version":"test"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let process = spawn_lifecycle_process_with_endpoint(&root, binary, endpoint.clone());
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        let capture = Arc::new(RecordingCapture::default());
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_secs(1),
                max_attempts: 2,
            },
            Some(capture.clone()),
        ));

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let lifecycle = session.lifecycle_snapshot();
                let hooks_completed = capture.releases.load(Ordering::Acquire) >= 1
                    && capture.reconciles.load(Ordering::Acquire) >= 1;
                let launches_completed =
                    std::fs::read_to_string(&launches).is_ok_and(|launches| launches.len() >= 2);
                if lifecycle.phase == CoreLifecyclePhase::Stable
                    && lifecycle.recovery_attempts == 1
                    && hooks_completed
                    && launches_completed
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("managed core recovery did not complete its observable effects");

        assert_eq!(capture.releases.load(Ordering::Acquire), 1);
        assert_eq!(capture.reconciles.load(Ordering::Acquire), 1);
        assert_eq!(std::fs::read_to_string(&launches).unwrap().len(), 2);
        assert!(process.is_running());
        session.shutdown().await.unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_during_configuration_validation_does_not_launch_a_candidate_or_recovery_child()
     {
        let _ports = fixed_listener_ports_guard().await;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = lifecycle_test_root("config-restart-shutdown");
        let source = root.join("source.yaml");
        let candidate = root.join("candidate.yaml");
        let launches = root.join("launches");
        let armed = root.join("armed");
        let validating = root.join("validating");
        let release = root.join("release");
        std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
        std::fs::write(&candidate, "mixed-port: 8012\nmode: rule\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&source, &[], CoreKind::Mihomo)
            .unwrap();
        let previous_cache = std::fs::read(store.runtime_path()).unwrap();
        let binary = root.join("mihomo");
        std::fs::write(&binary, format!("#!/bin/sh\nif [ \"$1\" = '-t' ]; then\n  if [ -f '{}' ]; then touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; fi\n  exit 0\nfi\nprintf x >> '{}'\ntrap 'exit 0' TERM INT\nwhile true; do sleep 0.01; done\n", armed.display(), validating.display(), release.display(), launches.display())).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let server_launches = launches.clone();
        let server = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { accepted = listener.accept() => accepted.unwrap(), _ = &mut stop_rx => break };
                let (mut stream, _) = accepted;
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0_u8; 4096];
                    let length = stream.read(&mut bytes).await.unwrap();
                    assert_ne!(length, 0);
                    request.extend_from_slice(&bytes[..length]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                if request.starts_with(b"PUT ") {
                    continue;
                } // A lost reload response forces managed restart.
                assert!(request.starts_with(b"GET /version "));
                tokio::time::timeout(Duration::from_secs(5), async {
                    while std::fs::read(&server_launches).unwrap().len() < 2 {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                let body = r#"{"meta":true,"version":"fixture"}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let process = MihomoProcess::spawn(MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: store.runtime_path(),
            home_dir: root.join("data"),
            endpoint: endpoint.clone(),
            controller_override: None,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !launches.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
            Some(source),
            vec![],
        )
        .unwrap();
        std::fs::write(&armed, []).unwrap();
        let apply_session = session.clone();
        let apply_store = store.clone();
        let apply = tokio::spawn(async move {
            apply_session
                .apply(
                    &apply_store,
                    EffectiveConfigIntent::ActivateProfile {
                        profile: candidate,
                        overrides: vec![],
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !validating.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        session.request_shutdown();
        let shutdown_session = session.clone();
        let shutdown = tokio::spawn(async move { shutdown_session.shutdown().await });
        std::fs::write(&release, []).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), apply)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        stop_tx.send(()).unwrap();
        server.await.unwrap();
        let launch_count = std::fs::read(&launches).unwrap().len();
        let cache = std::fs::read(store.runtime_path()).unwrap();
        let running = process.is_running();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            result.is_err(),
            "configuration restart was accepted after shutdown: {result:?}"
        );
        assert_eq!(
            launch_count, 1,
            "shutdown launched a candidate or recovery child"
        );
        assert_eq!(cache, previous_cache);
        assert!(!running);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_restarts_serialize_without_overlapping_children() {
        let root = lifecycle_test_root("serialized-restarts");
        let launches = root.join("launches");
        let guard = root.join("running");
        let overlaps = root.join("overlaps");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; if ! mkdir '{}' 2>/dev/null; then printf x >> '{}'; exit 99; fi; cleanup() {{ rmdir '{}' 2>/dev/null || true; }}; trap 'cleanup; exit 0' TERM INT; trap cleanup EXIT; while true; do sleep 0.05; done",
                launches.display(),
                guard.display(),
                overlaps.display(),
                guard.display(),
            ),
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let _ = stream.read(&mut request).unwrap();
                let body = r#"{"meta":true,"version":"test"}"#;
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let process = spawn_lifecycle_process_with_endpoint(&root, binary, endpoint.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !guard.is_dir() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initial managed child did not acquire its process guard");
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();

        let first = {
            let session = session.clone();
            tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
        };
        let second = {
            let session = session.clone();
            tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
        };
        let (first, second) = tokio::join!(first, second);

        let generations = [first.unwrap().unwrap(), second.unwrap().unwrap()];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !guard.is_dir() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("latest managed child did not acquire its process guard");
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();
        assert!(generations.contains(&1));
        assert!(generations.contains(&2));
        assert_eq!(session.snapshot().generation, 2);
        assert!(
            (2..=3).contains(&launch_count),
            "the initial and latest child must run; an immediately replaced child may not be scheduled"
        );
        assert!(!overlaps.exists(), "two managed children overlapped");
        assert!(process.is_running());
        session.shutdown().await.unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn lifecycle_test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("profile.yaml"), "rules:\n  - MATCH,DIRECT\n").unwrap();
        root
    }

    #[cfg(unix)]
    fn write_lifecycle_script(path: &std::path::Path, run: &str) {
        std::fs::write(
            path,
            format!("#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\n{run}\n"),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn spawn_lifecycle_process(root: &std::path::Path, binary: PathBuf) -> Arc<MihomoProcess> {
        spawn_lifecycle_process_with_endpoint(root, binary, MihomoEndpoint::default())
    }

    #[cfg(unix)]
    fn spawn_lifecycle_process_with_endpoint(
        root: &std::path::Path,
        binary: PathBuf,
        endpoint: MihomoEndpoint,
    ) -> Arc<MihomoProcess> {
        MihomoProcess::spawn(MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: root.join("profile.yaml"),
            home_dir: root.join("data"),
            endpoint,
            controller_override: None,
        })
        .unwrap()
    }

    #[cfg(unix)]
    #[derive(Default)]
    struct RecordingCapture {
        releases: AtomicUsize,
        reconciles: AtomicUsize,
    }

    #[cfg(unix)]
    impl CoreRecoveryCapture for RecordingCapture {
        fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
            self.releases.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }

        fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
            self.reconciles.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }
    }
}
