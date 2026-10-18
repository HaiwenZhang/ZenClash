//! Serialized, intent-oriented access to runtime-core transitions.

use std::{
    path::PathBuf,
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
    ControlledConfigError, ControlledConfigStore, CoreKind, CoreUpdateError, ManagedCoreProcess,
    MihomoClient, MihomoError, MihomoRelease, MihomoReleaseService, TrafficCaptureSession,
    VersionInfo,
    controlled_config::{
        RuntimeApplicationTransaction, RuntimeCandidateValidation, RuntimeMutationError,
        SavedRuntimeReceipt,
    },
    data_coordinator::DataWriteLease,
};

const CORE_READY_TIMEOUT: Duration = Duration::from_secs(20);

mod automatic;
mod backup_restore;
mod builtin_resources;
mod configuration_application;
mod maintenance;
mod supervision;
mod updates;
use backup_restore::snapshot_with_delta;
pub use builtin_resources::BuiltinResourceUpdateReceipt;
#[cfg(test)]
use supervision::supervisor_observation;
mod binding;
mod capture_recovery;
use capture_recovery::CoreRecoveryCapture;
mod observation;
mod observed_mutation;
mod resources;
pub use resources::{CoreResourceAction, ResourceActionReceipt};
mod permissions;
mod proxy_selection;
pub use proxy_selection::ProxyGroupActionReceipt;
mod runtime_revision;
pub use observation::{CoreObservation, CoreObservationIdentity};
pub(crate) use runtime_revision::RuntimeConfigurationRevision;
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

/// Admission for one persistent-data application and its runtime reconciliation.
///
/// Keep this guard alive through the completion task. Shutdown waits for every clone
/// before taking the runtime transition and stopping an owned kernel.
/// Repository writes still acquire their own data leases; this is completion
/// admission, not filesystem write authority.
#[derive(Clone)]
pub struct CoreDataApplicationAdmission {
    inner: Arc<DataApplicationAdmissionInner>,
}

/// Compatibility name for backup callers sharing the data-application admission.
pub type CoreBackupAdmission = CoreDataApplicationAdmission;

struct DataApplicationAdmissionInner {
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
    /// Capture validation, native operations or rollback failed.
    #[error(transparent)]
    Capture(#[from] Box<crate::TrafficCaptureError>),
    /// An external core would require a process restart ZenClash does not own.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "core_page.errors.external_restart_unsupported",
            &[("core", core.to_string())]
        )
    )]
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
    #[error("{}", zenclash_i18n::text("core_page.errors.shutting_down"))]
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
    data_application_gate: Arc<tokio::sync::Mutex<()>>,
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
    _availability_pause: Option<run_state::PacAvailabilityPauseGuard>,
}

enum CoreProfileApplicationState {
    Runtime {
        transaction: Box<RuntimeApplicationTransaction>,
        kind: CoreApplyKind,
    },
    Validated(RuntimeCandidateValidation),
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
            data_application_gate: Arc::new(tokio::sync::Mutex::new(())),
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

    /// Reserves one complete persistent-data application before the first live write.
    ///
    /// Transfer the guard immediately into an independently owned completion task.
    /// It covers persistence, runtime application and any compensation; shutdown
    /// waits for the entire completion, even after its foreground waiter leaves.
    ///
    /// # Errors
    /// Returns [`CoreSessionError::ShuttingDown`] when shutdown has started.
    pub async fn begin_data_application(
        &self,
    ) -> Result<CoreDataApplicationAdmission, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let guard = self.data_application_gate.clone().lock_owned().await;
        self.ensure_not_shutting_down()?;
        Ok(CoreDataApplicationAdmission {
            inner: Arc::new(DataApplicationAdmissionInner {
                _guard: guard,
                gate: self.data_application_gate.clone(),
            }),
        })
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

    pub(crate) fn write_scopes(&self) -> Vec<PathBuf> {
        self.write_scopes_for_client(&self.client)
    }

    fn write_scopes_for_client(&self, client: &MihomoClient) -> Vec<PathBuf> {
        client.write_scopes()
    }

    async fn acquire_runtime_write_lease(
        &self,
        client: &MihomoClient,
    ) -> Result<DataWriteLease, CoreSessionError> {
        let lease = DataWriteLease::shared_async(self.write_scopes_for_client(client))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        client.ensure_binding_current()?;
        Ok(lease)
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

    pub(crate) fn managed_process_snapshot(&self) -> Option<crate::CoreProcessSnapshot> {
        match self.client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => Some(process.snapshot()),
            _ => None,
        }
    }

    fn next_generation(&self) -> u64 {
        self.next_generation_with_config(None)
    }

    /// Invalidates ordinary runtime reads without changing the lifecycle classification.
    pub(crate) fn invalidate_runtime_observations(&self) -> u64 {
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

pub(crate) fn should_restart_after_hot_reload(error: &ControlledConfigError) -> bool {
    matches!(
        error,
        ControlledConfigError::Profile(
            MihomoError::Http(_)
                | MihomoError::ResponseDecode {
                    outcome_unknown: true,
                    ..
                }
        )
    )
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
mod tests;

#[cfg(test)]
pub(crate) mod test_support;
