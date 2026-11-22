//! Unified core-process launcher, lease, kernel-ownership, and leftover tracking abstractions.
//!
//! # Explicit 4-Level Lock Acquisition Order
//!
//! All concurrency gates in `zenclash-core` MUST be acquired strictly in ascending
//! level order to guarantee freedom from lock-ordering deadlocks:
//!
//! 1. **Level 1 — `CaptureGate`**: `TrafficCaptureSession` capture-plan serialization
//!    (`traffic_capture` operation mutex and `SystemProxyController` operation mutex).
//! 2. **Level 2 — `DataApplicationGate`**: cross-store business mutation and override-chain
//!    admission (`CoreSession::begin_data_application`).
//! 3. **Level 3 — `RuntimeTransitionGate` / `DataWriteLease`**: kernel start/stop/switch
//!    transitions (`CoreSession::transition`) and cross-directory persistent write leases
//!    (`data_coordinator::DataWriteLease`).
//! 4. **Level 4 — `ControllerMutationGate` / `MemoryLock`**: short critical-section
//!    controller writes (`MihomoClient::mutation_gate`) and pure in-memory locks
//!    (`parking_lot::Mutex`/`RwLock` that MUST NEVER be held across `.await` or blocking I/O).
//!
//! Acquiring a lower-numbered gate while holding a higher-numbered gate is strictly forbidden.

use std::{
    any::Any,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;

use crate::{
    CoreConfigValidator, CoreKind, MihomoEndpoint,
    core_errors::{MihomoError, MihomoResult},
    service::NativeController,
};

/// Boxed `Send` future returned by asynchronous launcher operations.
pub type LauncherFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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

impl CoreDataApplicationAdmission {
    pub(crate) fn new(
        guard: tokio::sync::OwnedMutexGuard<()>,
        gate: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        Self {
            inner: Arc::new(DataApplicationAdmissionInner {
                _guard: guard,
                gate,
            }),
        }
    }

    pub(crate) fn matches_gate(&self, gate: &Arc<tokio::sync::Mutex<()>>) -> bool {
        Arc::ptr_eq(&self.inner.gate, gate)
    }
}

#[derive(Clone)]
pub(crate) enum RuntimeRestoreAuthority {
    Ordinary,
    Backup(CoreBackupAdmission),
}

/// Controller ownership reported by the current in-memory runtime binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreRuntimeBackend {
    /// A controller whose process is outside ZenClash's ownership.
    Direct,
    /// A real child process owned by this application.
    Local,
    /// A kernel owned by the authenticated administrator service.
    Service,
}

/// Prepared runtime identity and immutable local launch paths.
///
/// Reading this descriptor performs no filesystem, process-status or IPC work.
/// Service paths are absent because protected helper copies are not user sources.
#[derive(Clone, Debug)]
pub struct CoreRuntimeDescriptor {
    pub(crate) binding_generation: u64,
    pub(crate) backend: CoreRuntimeBackend,
    pub(crate) kind: CoreKind,
    pub(crate) binary: Option<PathBuf>,
    pub(crate) config_file: Option<PathBuf>,
    pub(crate) home_dir: Option<PathBuf>,
}

impl CoreRuntimeDescriptor {
    /// Returns the transport generation pairing this identity with its launch metadata.
    #[must_use]
    pub const fn binding_generation(&self) -> u64 {
        self.binding_generation
    }
    /// Returns the process ownership boundary.
    #[must_use]
    pub const fn backend(&self) -> CoreRuntimeBackend {
        self.backend
    }
    /// Returns the actual runtime implementation.
    #[must_use]
    pub const fn kind(&self) -> CoreKind {
        self.kind
    }
    /// Returns the actual local binary, absent for service and external controllers.
    #[must_use]
    pub fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }
    /// Returns the actual local launch configuration path.
    #[must_use]
    pub fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }
    /// Returns the actual local process home.
    #[must_use]
    pub fn home_dir(&self) -> Option<&Path> {
        self.home_dir.as_deref()
    }
}

/// Immutable launch metadata exposed by a [`CoreProcessLease`] without tying transport
/// layers to concrete child-process implementations.
#[derive(Clone, Debug)]
pub struct CoreLaunchDescriptor {
    /// Runtime core implementation.
    pub kind: CoreKind,
    /// Resolved executable path.
    pub binary: PathBuf,
    /// Original YAML configuration source path.
    pub config_file: PathBuf,
    /// Writable runtime home directory passed via `-d`.
    pub home_dir: PathBuf,
    /// Controller endpoint bound to this process.
    pub endpoint: MihomoEndpoint,
    /// Optional explicit `-ext-ctl` address override.
    pub controller_override: Option<String>,
}

/// Unified ownership lease over one managed core process (local child or service helper).
///
/// Transport clients (`MihomoClient` / `ControllerBinding`) depend on this trait rather
/// than importing concrete process management types directly.
pub trait CoreProcessLease: Send + Sync + 'static {
    /// Returns the runtime core implementation owned by this lease.
    fn kind(&self) -> CoreKind;

    /// Returns the controller endpoint associated with this lease.
    fn endpoint(&self) -> MihomoEndpoint;

    /// Returns the immutable launch descriptor for diagnostics and local recovery.
    fn launch_descriptor(&self) -> CoreLaunchDescriptor;

    /// Returns filesystem write scopes authorized by this managed runtime.
    fn write_scopes(&self) -> Vec<PathBuf>;

    /// Builds a configuration validator matching this managed runtime.
    fn config_validator(&self) -> CoreConfigValidator;

    /// Returns a native local IPC controller when this lease uses `ipc://`.
    ///
    /// # Errors
    ///
    /// Returns an error if the managed process is stopped or its IPC controller
    /// cannot be resolved.
    fn native_controller(&self) -> MihomoResult<Option<NativeController>>;

    /// Stops the underlying managed core process and confirms exit where supported.
    ///
    /// # Errors
    ///
    /// Returns an error if termination or exit confirmation fails.
    fn stop_process(&self) -> MihomoResult<()>;

    /// Returns the operating-system process identifier if currently running.
    fn pid(&self) -> Option<u32>;

    /// Reports whether the leased core process is currently observed running.
    fn is_running(&self) -> bool;

    /// Upcasts this `Arc` to `Any` so higher runtime layers can recover the concrete
    /// process type without introducing a dependency from transport to `managed_core_process`.
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

/// Abstract process launcher unifying zero-privilege local spawning and thin privileged
/// helper spawning behind one contract.
pub trait CoreProcessLauncher: Send + Sync {
    /// Stable diagnostic identifier for this launcher backend (for example `"local"` or `"service"`).
    fn backend_name(&self) -> &'static str;

    /// Starts a managed core process for the supplied descriptor and returns its lease.
    fn launch<'a>(
        &'a self,
        descriptor: &'a CoreLaunchDescriptor,
    ) -> LauncherFuture<'a, MihomoResult<Arc<dyn CoreProcessLease>>>;
}

/// High-level kernel ownership boundary decoupled from transport internals.
#[derive(Clone)]
pub enum KernelOwner {
    /// External controller whose process lifecycle is outside ZenClash's ownership.
    External(MihomoEndpoint),
    /// Managed kernel process owned through a unified [`CoreProcessLease`].
    Managed(Arc<dyn CoreProcessLease>),
}

impl std::fmt::Debug for KernelOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::External(endpoint) => formatter.debug_tuple("External").field(endpoint).finish(),
            Self::Managed(lease) => formatter
                .debug_struct("Managed")
                .field("kind", &lease.kind())
                .field("pid", &lease.pid())
                .field("running", &lease.is_running())
                .finish(),
        }
    }
}

impl KernelOwner {
    /// Returns the underlying process lease when ZenClash owns the kernel.
    #[must_use]
    pub fn managed_lease(&self) -> Option<&Arc<dyn CoreProcessLease>> {
        match self {
            Self::External(_) => None,
            Self::Managed(lease) => Some(lease),
        }
    }

    /// Returns whether ZenClash owns the kernel process lifecycle.
    #[must_use]
    pub fn is_managed(&self) -> bool {
        matches!(self, Self::Managed(_))
    }
}

/// Record of an old managed core lease whose exit was not yet confirmed.
///
/// Subsequent start intents MUST reclaim all registered leftover leases before
/// spawning a replacement kernel, preventing dual-kernel coexistence.
#[derive(Clone)]
pub struct LeftoverLease {
    id: u64,
    last_pid: Option<u32>,
    lease: Arc<dyn CoreProcessLease>,
}

impl std::fmt::Debug for LeftoverLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeftoverLease")
            .field("id", &self.id)
            .field("last_pid", &self.last_pid)
            .field("running", &self.lease.is_running())
            .finish()
    }
}

impl LeftoverLease {
    /// Returns the monotonically assigned leftover registration identifier.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the process identifier observed when the leftover was registered.
    #[must_use]
    pub const fn last_pid(&self) -> Option<u32> {
        self.last_pid
    }

    /// Attempts to confirm exit or stop the underlying leftover process.
    ///
    /// # Errors
    ///
    /// Returns the stop failure when exit cannot yet be confirmed.
    pub fn try_reclaim(&self) -> MihomoResult<()> {
        if !self.lease.is_running() {
            return Ok(());
        }
        self.lease.stop_process()
    }
}

/// Thread-safe registry of unconfirmed-exit kernel leases that block new kernel launches
/// until successfully reclaimed.
#[derive(Clone, Default)]
pub struct LeftoverLeaseTracker {
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<Vec<LeftoverLease>>>,
}

impl std::fmt::Debug for LeftoverLeaseTracker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeftoverLeaseTracker")
            .field("pending_count", &self.pending_count())
            .finish()
    }
}

impl LeftoverLeaseTracker {
    /// Creates an empty leftover tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a lease whose termination was not confirmed, returning its tracking id.
    pub fn register(&self, lease: Arc<dyn CoreProcessLease>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let last_pid = lease.pid();
        self.pending.lock().push(LeftoverLease {
            id,
            last_pid,
            lease,
        });
        id
    }

    /// Reports whether any unconfirmed leftover lease is still pending reclamation.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    /// Returns the number of currently pending leftover leases.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Attempts to reclaim all pending leftover leases without holding the registry lock
    /// across potentially blocking `stop_process` calls.
    ///
    /// Successfully reclaimed leases are removed; any lease that still fails to confirm
    /// exit remains registered and causes this method to return an error.
    ///
    /// # Errors
    ///
    /// Returns an error if one or more leftover leases could not be confirmed stopped.
    pub fn reclaim_all(&self) -> MihomoResult<()> {
        let snapshot = {
            let guard = self.pending.lock();
            if guard.is_empty() {
                return Ok(());
            }
            guard.clone()
        };
        let mut reclaimed_ids = Vec::with_capacity(snapshot.len());
        let mut first_error = None;
        for entry in snapshot {
            match entry.try_reclaim() {
                Ok(()) => reclaimed_ids.push(entry.id),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if !reclaimed_ids.is_empty() {
            self.pending
                .lock()
                .retain(|entry| !reclaimed_ids.contains(&entry.id));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Rejects starting a new kernel if any leftover lease remains unreclaimed after
    /// attempting cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if any leftover lease cannot be confirmed exited.
    pub fn ensure_reclaimed_before_start(&self) -> MihomoResult<()> {
        self.reclaim_all()?;
        if self.has_pending() {
            return Err(MihomoError::Process(
                "previous managed core lease is still active; cannot start a second core".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::*;

    struct FakeLease {
        running: AtomicBool,
        fail_stop: AtomicBool,
        stop_calls: AtomicUsize,
        pid: Option<u32>,
    }

    impl FakeLease {
        fn new(running: bool, fail_stop: bool, pid: Option<u32>) -> Arc<Self> {
            Arc::new(Self {
                running: AtomicBool::new(running),
                fail_stop: AtomicBool::new(fail_stop),
                stop_calls: AtomicUsize::new(0),
                pid,
            })
        }
    }

    impl CoreProcessLease for FakeLease {
        fn kind(&self) -> CoreKind {
            CoreKind::Mihomo
        }

        fn endpoint(&self) -> MihomoEndpoint {
            MihomoEndpoint::default()
        }

        fn launch_descriptor(&self) -> CoreLaunchDescriptor {
            CoreLaunchDescriptor {
                kind: CoreKind::Mihomo,
                binary: PathBuf::from("/fake/mihomo"),
                config_file: PathBuf::from("/fake/config.yaml"),
                home_dir: PathBuf::from("/fake"),
                endpoint: MihomoEndpoint::default(),
                controller_override: None,
            }
        }

        fn write_scopes(&self) -> Vec<PathBuf> {
            vec![PathBuf::from("/fake")]
        }

        fn config_validator(&self) -> CoreConfigValidator {
            CoreConfigValidator::new(
                CoreKind::Mihomo,
                PathBuf::from("/fake/mihomo"),
                PathBuf::from("/fake"),
            )
        }

        fn native_controller(&self) -> MihomoResult<Option<NativeController>> {
            Ok(None)
        }

        fn stop_process(&self) -> MihomoResult<()> {
            self.stop_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_stop.load(Ordering::Acquire) {
                return Err(MihomoError::Process("stop still pending".into()));
            }
            self.running.store(false, Ordering::Release);
            Ok(())
        }

        fn pid(&self) -> Option<u32> {
            self.running
                .load(Ordering::Acquire)
                .then_some(self.pid)
                .flatten()
        }

        fn is_running(&self) -> bool {
            self.running.load(Ordering::Acquire)
        }

        fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    #[test]
    fn leftover_tracker_blocks_new_start_until_previous_lease_exits() {
        let tracker = LeftoverLeaseTracker::new();
        let lease = FakeLease::new(true, true, Some(4242));
        let id = tracker.register(lease.clone());
        assert_eq!(id, 1);
        assert!(tracker.has_pending());
        assert!(tracker.ensure_reclaimed_before_start().is_err());
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 1);

        lease.fail_stop.store(false, Ordering::Release);
        tracker.ensure_reclaimed_before_start().unwrap();
        assert!(!tracker.has_pending());
        assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn leftover_tracker_skips_stop_for_already_exited_lease() {
        let tracker = LeftoverLeaseTracker::new();
        let lease = FakeLease::new(false, true, None);
        tracker.register(lease.clone());
        tracker.reclaim_all().unwrap();
        assert!(!tracker.has_pending());
        assert_eq!(lease.stop_calls.load(Ordering::Relaxed), 0);
    }
}
