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
    mihomo::errors::{MihomoError, MihomoResult},
    runtime::service::ipc::NativeController,
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

/// Correlates a runtime binding with its accepted configuration generation.
/// These counters do not identify the owning session or authorize a mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeConfigurationRevision {
    pub(crate) binding_generation: u64,
    pub(crate) configuration_generation: u64,
}

#[derive(Clone)]
pub(crate) enum ProfileRuntimeSource {
    File(PathBuf),
    Frozen(Arc<str>),
    Prepared(Box<(crate::ControlledConfigUpdate, Option<String>)>),
}

impl From<PathBuf> for ProfileRuntimeSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

type LaunchCallback = Arc<
    dyn Fn(CoreLaunchDescriptor) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
        + Send
        + Sync,
>;

async fn ensure_reclaimed_before_start_blocking(
    leftovers: &LeftoverLeaseTracker,
) -> MihomoResult<()> {
    if !leftovers.has_pending() {
        return Ok(());
    }
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || tracker.ensure_reclaimed_before_start())
        .await
        .map_err(|error| {
            MihomoError::Process(format!("leftover reclamation task failed: {error}"))
        })?
}

async fn reclaim_all_blocking(leftovers: &LeftoverLeaseTracker) -> MihomoResult<()> {
    if !leftovers.has_pending() {
        return Ok(());
    }
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || tracker.reclaim_all())
        .await
        .map_err(|error| {
            MihomoError::Process(format!("leftover reclamation task failed: {error}"))
        })?
}

async fn stop_lease_or_register_leftover(
    leftovers: &LeftoverLeaseTracker,
    lease: Arc<dyn CoreProcessLease>,
) -> MihomoResult<()> {
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || {
        if !lease.is_running() {
            return Ok(());
        }
        if let Err(error) = lease.stop_process() {
            tracker.register(lease);
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|error| MihomoError::Process(format!("managed core stop task failed: {error}")))?
}

fn blocking_spawn_callback<F>(spawn: F) -> LaunchCallback
where
    F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>> + Send + Sync + 'static,
{
    let spawn = Arc::new(spawn);
    Arc::new(move |descriptor| {
        let spawn = Arc::clone(&spawn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || spawn(descriptor))
                .await
                .map_err(|error| {
                    MihomoError::Process(format!("core process launch task failed: {error}"))
                })?
        })
    })
}

/// Concrete zero-privilege local [`CoreProcessLauncher`] adapter gated by a
/// [`LeftoverLeaseTracker`].
///
/// Before spawning a replacement child process, `LocalLauncher` reclaims any
/// pending leftover leases on a blocking worker thread so two local cores can
/// never coexist.
#[derive(Clone)]
pub struct LocalLauncher {
    leftovers: LeftoverLeaseTracker,
    spawn: LaunchCallback,
}

impl std::fmt::Debug for LocalLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalLauncher")
            .field("backend", &self.backend_name())
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl LocalLauncher {
    /// Creates a concrete zero-privilege local launcher backed by
    /// [`crate::runtime::local::process::ManagedCoreProcess::spawn`] executed on a blocking worker thread.
    #[must_use]
    pub fn managed(leftovers: LeftoverLeaseTracker) -> Self {
        Self::with_blocking(leftovers, |descriptor| {
            let process = crate::runtime::local::process::ManagedCoreProcess::spawn(
                crate::runtime::local::process::CoreLaunchConfig::from(descriptor),
            )?;
            Ok(process as Arc<dyn CoreProcessLease>)
        })
    }

    /// Creates a `LocalLauncher` backed by an asynchronous launch callback.
    #[must_use]
    pub fn new<F>(leftovers: LeftoverLeaseTracker, spawn: F) -> Self
    where
        F: Fn(
                CoreLaunchDescriptor,
            ) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            leftovers,
            spawn: Arc::new(spawn),
        }
    }

    /// Creates a `LocalLauncher` backed by a blocking process-spawn callback executed
    /// inside [`tokio::task::spawn_blocking`].
    #[must_use]
    pub fn with_blocking<F>(leftovers: LeftoverLeaseTracker, spawn: F) -> Self
    where
        F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            leftovers,
            spawn: blocking_spawn_callback(spawn),
        }
    }

    /// Returns the leftover-lease tracker guarding this launcher.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }
}

impl CoreProcessLauncher for LocalLauncher {
    fn backend_name(&self) -> &'static str {
        "local"
    }

    fn launch<'a>(
        &'a self,
        descriptor: &'a CoreLaunchDescriptor,
    ) -> LauncherFuture<'a, MihomoResult<Arc<dyn CoreProcessLease>>> {
        let descriptor = descriptor.clone();
        let leftovers = self.leftovers.clone();
        let spawn = Arc::clone(&self.spawn);
        Box::pin(async move {
            ensure_reclaimed_before_start_blocking(&leftovers).await?;
            spawn(descriptor).await
        })
    }
}

/// Concrete thin privileged-service [`CoreProcessLauncher`] adapter gated by a
/// [`LeftoverLeaseTracker`].
///
/// Encapsulates the user-readable application home and optional trusted core path
/// shared with the privileged helper, ensuring leftover reclamation precedes every
/// launch request.
#[derive(Clone)]
pub struct ServiceLauncher {
    source_home: PathBuf,
    core_source: Option<PathBuf>,
    leftovers: LeftoverLeaseTracker,
    spawn: LaunchCallback,
}

impl std::fmt::Debug for ServiceLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceLauncher")
            .field("backend", &self.backend_name())
            .field("source_home", &self.source_home)
            .field("core_source", &self.core_source)
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl ServiceLauncher {
    /// Creates a `ServiceLauncher` backed by an asynchronous helper launch callback.
    #[must_use]
    pub fn new<F>(
        source_home: PathBuf,
        core_source: Option<PathBuf>,
        leftovers: LeftoverLeaseTracker,
        spawn: F,
    ) -> Self
    where
        F: Fn(
                CoreLaunchDescriptor,
            ) -> LauncherFuture<'static, MihomoResult<Arc<dyn CoreProcessLease>>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            source_home,
            core_source,
            leftovers,
            spawn: Arc::new(spawn),
        }
    }

    /// Creates a `ServiceLauncher` backed by a blocking helper launch callback executed
    /// inside [`tokio::task::spawn_blocking`].
    #[must_use]
    pub fn with_blocking<F>(
        source_home: PathBuf,
        core_source: Option<PathBuf>,
        leftovers: LeftoverLeaseTracker,
        spawn: F,
    ) -> Self
    where
        F: Fn(CoreLaunchDescriptor) -> MihomoResult<Arc<dyn CoreProcessLease>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            source_home,
            core_source,
            leftovers,
            spawn: blocking_spawn_callback(spawn),
        }
    }

    /// Returns the user-readable application home directory bound to this service launcher.
    #[must_use]
    pub fn source_home(&self) -> &Path {
        &self.source_home
    }

    /// Returns the optional selected core executable path for trusted-path verification.
    #[must_use]
    pub fn core_source(&self) -> Option<&Path> {
        self.core_source.as_deref()
    }

    /// Returns the leftover-lease tracker guarding this launcher.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }
}

impl CoreProcessLauncher for ServiceLauncher {
    fn backend_name(&self) -> &'static str {
        "service"
    }

    fn launch<'a>(
        &'a self,
        descriptor: &'a CoreLaunchDescriptor,
    ) -> LauncherFuture<'a, MihomoResult<Arc<dyn CoreProcessLease>>> {
        let descriptor = descriptor.clone();
        let leftovers = self.leftovers.clone();
        let spawn = Arc::clone(&self.spawn);
        Box::pin(async move {
            ensure_reclaimed_before_start_blocking(&leftovers).await?;
            spawn(descriptor).await
        })
    }
}

/// Desired lifecycle target state driven by [`IntentReconciler`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesiredCoreIntent {
    /// Managed core should be running with the supplied launch descriptor.
    Running(Box<CoreLaunchDescriptor>),
    /// Managed core should be stopped while keeping the reconciler open for future intents.
    Stopped,
    /// Managed core should be stopped permanently; subsequent non-closed intents are rejected.
    Closed,
}

impl DesiredCoreIntent {
    /// Builds a [`DesiredCoreIntent::Running`] intent from a launch descriptor.
    #[must_use]
    pub fn running(descriptor: CoreLaunchDescriptor) -> Self {
        Self::Running(Box::new(descriptor))
    }

    /// Reports whether this intent requests a running managed core.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// Reports whether this intent requests a stopped core while remaining open.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Reports whether this intent permanently closes the reconciler.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Result of reconciling a [`DesiredCoreIntent`] revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// This revision executed a runtime transition and became the committed state.
    Applied {
        /// Monotonic revision applied by the reconciler.
        revision: u64,
    },
    /// The runtime already satisfied the desired intent, so no redundant transition ran.
    Coalesced {
        /// Monotonic revision satisfied without a transition.
        revision: u64,
    },
    /// A newer desired intent arrived before this revision could commit.
    Superseded {
        /// Older revision that was overtaken.
        revision: u64,
        /// Latest monotonic revision that superseded it.
        latest_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommittedCoreState {
    Stopped,
    Running(Box<CoreLaunchDescriptor>),
    Closed,
}

struct ReconcilerState {
    latest_revision: u64,
    applied_revision: u64,
    desired: DesiredCoreIntent,
    closed_requested: bool,
    committed: CommittedCoreState,
    active_lease: Option<Arc<dyn CoreProcessLease>>,
}

/// Single-worker latest-desired-intent reconciler with monotonic `revision` tracking
/// and integrated [`LeftoverLeaseTracker`] gating.
///
/// Instead of executing every queued start/stop/restart command serially, callers
/// submit desired lifecycle targets (`Running | Stopped | Closed`). Rapid intermediate
/// intents are coalesced or superseded before wasting launches, and any managed lease
/// whose exit cannot be confirmed is registered in [`LeftoverLeaseTracker`] to block
/// subsequent `Running` launches until exit is confirmed.
#[derive(Clone)]
pub struct IntentReconciler {
    worker_gate: Arc<tokio::sync::Mutex<()>>,
    state: Arc<Mutex<ReconcilerState>>,
    leftovers: LeftoverLeaseTracker,
}

impl std::fmt::Debug for IntentReconciler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (latest_revision, applied_revision, closed, running) = {
            let state = self.state.lock();
            (
                state.latest_revision,
                state.applied_revision,
                state.closed_requested,
                state.active_lease.is_some(),
            )
        };
        formatter
            .debug_struct("IntentReconciler")
            .field("latest_revision", &latest_revision)
            .field("applied_revision", &applied_revision)
            .field("closed", &closed)
            .field("has_active_lease", &running)
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl Default for IntentReconciler {
    fn default() -> Self {
        Self::new(LeftoverLeaseTracker::new())
    }
}

impl IntentReconciler {
    /// Creates a new reconciler initially in the `Stopped` state, sharing the
    /// provided [`LeftoverLeaseTracker`].
    #[must_use]
    pub fn new(leftovers: LeftoverLeaseTracker) -> Self {
        Self {
            worker_gate: Arc::new(tokio::sync::Mutex::new(())),
            state: Arc::new(Mutex::new(ReconcilerState {
                latest_revision: 0,
                applied_revision: 0,
                desired: DesiredCoreIntent::Stopped,
                closed_requested: false,
                committed: CommittedCoreState::Stopped,
                active_lease: None,
            })),
            leftovers,
        }
    }

    /// Returns the latest monotonically assigned desired-intent revision.
    #[must_use]
    pub fn latest_revision(&self) -> u64 {
        self.state.lock().latest_revision
    }

    /// Returns the most recent revision confirmed as applied or coalesced.
    #[must_use]
    pub fn applied_revision(&self) -> u64 {
        self.state.lock().applied_revision
    }

    /// Returns a clone of the latest desired intent recorded by this reconciler.
    #[must_use]
    pub fn desired_intent(&self) -> DesiredCoreIntent {
        self.state.lock().desired.clone()
    }

    /// Returns the currently committed active process lease, if any.
    #[must_use]
    pub fn active_lease(&self) -> Option<Arc<dyn CoreProcessLease>> {
        self.state.lock().active_lease.clone()
    }

    /// Reports whether permanent shutdown (`Closed`) has been requested.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.lock().closed_requested
    }

    /// Returns the [`LeftoverLeaseTracker`] integrated with this reconciler.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }

    /// Records a new latest desired intent and returns its monotonically increasing
    /// revision without waiting for the worker gate.
    ///
    /// # Errors
    ///
    /// Returns an error if [`DesiredCoreIntent::Closed`] has already been submitted
    /// and a non-closed intent is requested.
    pub fn submit(&self, intent: DesiredCoreIntent) -> MihomoResult<u64> {
        let mut state = self.state.lock();
        if state.closed_requested && !intent.is_closed() {
            return Err(MihomoError::Process(
                "core intent reconciler is closed; refusing new runtime intent".into(),
            ));
        }
        if intent.is_closed() {
            state.closed_requested = true;
        }
        state.latest_revision = state.latest_revision.saturating_add(1);
        state.desired = intent;
        Ok(state.latest_revision)
    }

    /// Submits `intent` as a new revision and advances the reconciler for that revision.
    ///
    /// Returns:
    /// - [`ReconcileOutcome::Applied`] when this revision executed the state transition;
    /// - [`ReconcileOutcome::Coalesced`] when the runtime already satisfied `intent`;
    /// - [`ReconcileOutcome::Superseded`] when a newer revision overtook this one.
    ///
    /// # Errors
    ///
    /// Returns an error if the reconciler is already closed for non-closed intents,
    /// if an existing or leftover lease cannot be confirmed exited before starting,
    /// or if the launcher fails to start the core process.
    pub async fn reconcile(
        &self,
        intent: DesiredCoreIntent,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let revision = self.submit(intent)?;
        self.reconcile_revision(Some(revision), launcher).await
    }

    /// Executes one single-worker reconciliation step toward whichever desired intent
    /// revision is currently latest.
    ///
    /// # Errors
    ///
    /// Returns an error if stopping an existing lease, reclaiming leftovers, or
    /// launching the target core fails.
    pub async fn reconcile_latest(
        &self,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        self.reconcile_revision(None, launcher).await
    }

    async fn reconcile_revision(
        &self,
        pinned_revision: Option<u64>,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let _worker = self.worker_gate.lock().await;

        let (target_revision, target_intent, current_committed, current_lease) = {
            let state = self.state.lock();
            if let Some(revision) = pinned_revision
                && state.latest_revision > revision
            {
                return Ok(ReconcileOutcome::Superseded {
                    revision,
                    latest_revision: state.latest_revision,
                });
            }
            (
                pinned_revision.unwrap_or(state.latest_revision),
                state.desired.clone(),
                state.committed.clone(),
                state.active_lease.clone(),
            )
        };

        match target_intent {
            DesiredCoreIntent::Stopped | DesiredCoreIntent::Closed => {
                let target_committed = if target_intent.is_closed() {
                    CommittedCoreState::Closed
                } else {
                    CommittedCoreState::Stopped
                };
                self.reconcile_stop_or_close(
                    target_revision,
                    target_committed,
                    current_committed,
                    current_lease,
                )
                .await
            }
            DesiredCoreIntent::Running(descriptor) => {
                self.reconcile_running(
                    target_revision,
                    descriptor,
                    current_committed,
                    current_lease,
                    launcher,
                )
                .await
            }
        }
    }

    async fn reconcile_stop_or_close(
        &self,
        target_revision: u64,
        target_committed: CommittedCoreState,
        current_committed: CommittedCoreState,
        current_lease: Option<Arc<dyn CoreProcessLease>>,
    ) -> MihomoResult<ReconcileOutcome> {
        let mut did_work = current_committed != target_committed;

        if let Some(lease) = current_lease {
            did_work = true;
            {
                let mut state = self.state.lock();
                state.active_lease = None;
                if state.committed != CommittedCoreState::Closed {
                    state.committed = CommittedCoreState::Stopped;
                }
            }
            stop_lease_or_register_leftover(&self.leftovers, lease).await?;
        }

        if self.leftovers.has_pending() {
            did_work = true;
            reclaim_all_blocking(&self.leftovers).await?;
        }

        let mut state = self.state.lock();
        state.committed = target_committed;
        if state.latest_revision > target_revision {
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision: state.latest_revision,
            });
        }
        state.applied_revision = target_revision;
        if did_work {
            Ok(ReconcileOutcome::Applied {
                revision: target_revision,
            })
        } else {
            Ok(ReconcileOutcome::Coalesced {
                revision: target_revision,
            })
        }
    }

    async fn reconcile_running(
        &self,
        target_revision: u64,
        descriptor: Box<CoreLaunchDescriptor>,
        current_committed: CommittedCoreState,
        current_lease: Option<Arc<dyn CoreProcessLease>>,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let already_running_same = current_committed
            == CommittedCoreState::Running(descriptor.clone())
            && current_lease
                .as_ref()
                .is_some_and(|lease| lease.is_running())
            && !self.leftovers.has_pending();

        if already_running_same {
            let mut state = self.state.lock();
            if state.latest_revision > target_revision {
                return Ok(ReconcileOutcome::Superseded {
                    revision: target_revision,
                    latest_revision: state.latest_revision,
                });
            }
            state.applied_revision = target_revision;
            return Ok(ReconcileOutcome::Coalesced {
                revision: target_revision,
            });
        }

        if let Some(old_lease) = current_lease {
            {
                let mut state = self.state.lock();
                state.active_lease = None;
                if state.committed != CommittedCoreState::Closed {
                    state.committed = CommittedCoreState::Stopped;
                }
            }
            stop_lease_or_register_leftover(&self.leftovers, old_lease).await?;
        }

        ensure_reclaimed_before_start_blocking(&self.leftovers).await?;

        if let Some(latest_revision) = self.check_superseded(target_revision) {
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision,
            });
        }

        let new_lease = launcher.launch(&descriptor).await?;

        let superseded_by = {
            let mut state = self.state.lock();
            if state.latest_revision > target_revision || state.closed_requested {
                Some(state.latest_revision)
            } else {
                state.active_lease = Some(Arc::clone(&new_lease));
                state.committed = CommittedCoreState::Running(descriptor);
                state.applied_revision = target_revision;
                None
            }
        };

        if let Some(latest_revision) = superseded_by {
            let _ = stop_lease_or_register_leftover(&self.leftovers, new_lease).await;
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision,
            });
        }

        Ok(ReconcileOutcome::Applied {
            revision: target_revision,
        })
    }

    fn check_superseded(&self, target_revision: u64) -> Option<u64> {
        let state = self.state.lock();
        (state.latest_revision > target_revision || state.closed_requested)
            .then_some(state.latest_revision)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use tokio::sync::Notify;

    use super::*;

    struct FakeLease {
        running: AtomicBool,
        fail_stop: AtomicBool,
        stop_calls: AtomicUsize,
        pid: Option<u32>,
        descriptor: CoreLaunchDescriptor,
    }

    impl FakeLease {
        fn new(running: bool, fail_stop: bool, pid: Option<u32>) -> Arc<Self> {
            Self::with_descriptor(running, fail_stop, pid, sample_descriptor("default"))
        }

        fn with_descriptor(
            running: bool,
            fail_stop: bool,
            pid: Option<u32>,
            descriptor: CoreLaunchDescriptor,
        ) -> Arc<Self> {
            Arc::new(Self {
                running: AtomicBool::new(running),
                fail_stop: AtomicBool::new(fail_stop),
                stop_calls: AtomicUsize::new(0),
                pid,
                descriptor,
            })
        }
    }

    fn sample_descriptor(tag: &str) -> CoreLaunchDescriptor {
        CoreLaunchDescriptor {
            kind: CoreKind::Mihomo,
            binary: PathBuf::from(format!("/fake/{tag}/mihomo")),
            config_file: PathBuf::from(format!("/fake/{tag}/config.yaml")),
            home_dir: PathBuf::from(format!("/fake/{tag}")),
            endpoint: MihomoEndpoint::new(format!("http://127.0.0.1:9090/{tag}"), "secret"),
            controller_override: None,
        }
    }

    impl CoreProcessLease for FakeLease {
        fn kind(&self) -> CoreKind {
            self.descriptor.kind
        }

        fn endpoint(&self) -> MihomoEndpoint {
            self.descriptor.endpoint.clone()
        }

        fn launch_descriptor(&self) -> CoreLaunchDescriptor {
            self.descriptor.clone()
        }

        fn write_scopes(&self) -> Vec<PathBuf> {
            vec![self.descriptor.home_dir.clone()]
        }

        fn config_validator(&self) -> CoreConfigValidator {
            CoreConfigValidator::new(
                self.descriptor.kind,
                self.descriptor.binary.clone(),
                self.descriptor.home_dir.clone(),
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

    #[tokio::test]
    async fn local_and_service_launchers_gate_on_leftovers_before_spawning() {
        let leftovers = LeftoverLeaseTracker::new();
        let stuck = FakeLease::new(true, true, Some(1001));
        leftovers.register(stuck.clone());

        let local_spawns = Arc::new(AtomicUsize::new(0));
        let local_counter = Arc::clone(&local_spawns);
        let local = LocalLauncher::with_blocking(leftovers.clone(), move |descriptor| {
            local_counter.fetch_add(1, Ordering::Relaxed);
            Ok(FakeLease::with_descriptor(
                true,
                false,
                Some(2001),
                descriptor,
            ))
        });
        assert_eq!(local.backend_name(), "local");

        let service_spawns = Arc::new(AtomicUsize::new(0));
        let service_counter = Arc::clone(&service_spawns);
        let service = ServiceLauncher::with_blocking(
            PathBuf::from("/fake/home"),
            Some(PathBuf::from("/fake/mihomo")),
            leftovers.clone(),
            move |descriptor| {
                service_counter.fetch_add(1, Ordering::Relaxed);
                Ok(FakeLease::with_descriptor(
                    true,
                    false,
                    Some(3001),
                    descriptor,
                ))
            },
        );
        assert_eq!(service.backend_name(), "service");
        assert_eq!(service.source_home(), Path::new("/fake/home"));
        assert_eq!(service.core_source(), Some(Path::new("/fake/mihomo")));

        let desc = sample_descriptor("alpha");
        assert!(local.launch(&desc).await.is_err());
        assert!(service.launch(&desc).await.is_err());
        assert_eq!(local_spawns.load(Ordering::Relaxed), 0);
        assert_eq!(service_spawns.load(Ordering::Relaxed), 0);

        // Allow leftover to exit; both launchers now succeed cleanly.
        stuck.fail_stop.store(false, Ordering::Release);
        let local_lease = local.launch(&desc).await.unwrap();
        assert_eq!(local_lease.pid(), Some(2001));
        assert_eq!(local_spawns.load(Ordering::Relaxed), 1);

        let service_lease = service.launch(&desc).await.unwrap();
        assert_eq!(service_lease.pid(), Some(3001));
        assert_eq!(service_spawns.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn reconciler_coalesces_redundant_intents_and_restarts_on_crash_or_change() {
        let leftovers = LeftoverLeaseTracker::new();
        let reconciler = IntentReconciler::new(leftovers.clone());
        let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
        let launched_ref = Arc::clone(&launched);
        let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
            let lease = FakeLease::with_descriptor(true, false, Some(500), descriptor);
            launched_ref.lock().push(Arc::clone(&lease));
            Ok(lease)
        });

        // Compile-time check that reconcile futures remain Send.
        fn assert_send<T: Send>(_: &T) {}
        let fut = reconciler.reconcile(DesiredCoreIntent::Stopped, &launcher);
        assert_send(&fut);
        assert_eq!(
            fut.await.unwrap(),
            ReconcileOutcome::Coalesced { revision: 1 }
        );

        // First Running applies.
        let desc_a = sample_descriptor("a");
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::running(desc_a.clone()), &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 2 }
        );
        assert_eq!(launched.lock().len(), 1);

        // Identical Running while lease is alive coalesces without restarting.
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::running(desc_a.clone()), &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Coalesced { revision: 3 }
        );
        assert_eq!(launched.lock().len(), 1);

        // If the lease crashes externally, identical Running does NOT coalesce; it relaunches.
        launched.lock()[0].running.store(false, Ordering::Release);
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::running(desc_a), &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 4 }
        );
        assert_eq!(launched.lock().len(), 2);

        // Changing descriptor stops old lease and launches new one.
        let desc_b = sample_descriptor("b");
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::running(desc_b), &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 5 }
        );
        assert_eq!(launched.lock().len(), 3);
        assert!(!launched.lock()[1].is_running());
        assert!(launched.lock()[2].is_running());
    }

    #[tokio::test]
    async fn reconciler_supersedes_queued_and_in_flight_intents_without_leaking_leases() {
        let leftovers = LeftoverLeaseTracker::new();
        let reconciler = IntentReconciler::new(leftovers.clone());

        let first_launch_entered = Arc::new(Notify::new());
        let release_first_launch = Arc::new(Notify::new());
        let launched_tags = Arc::new(Mutex::new(Vec::<(String, Arc<FakeLease>)>::new()));

        let entered = Arc::clone(&first_launch_entered);
        let release = Arc::clone(&release_first_launch);
        let tags = Arc::clone(&launched_tags);
        let call_count = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&call_count);

        let launcher = Arc::new(LocalLauncher::new(leftovers, move |descriptor| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let tags = Arc::clone(&tags);
            let n = calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if n == 0 {
                    entered.notify_one();
                    release.notified().await;
                }
                let tag = descriptor.home_dir.to_string_lossy().into_owned();
                let lease =
                    FakeLease::with_descriptor(true, false, Some(600 + n as u32), descriptor);
                tags.lock().push((tag, Arc::clone(&lease)));
                Ok(lease as Arc<dyn CoreProcessLease>)
            })
        }));

        // Start rev 1 (blocks inside launch).
        let r1 = reconciler.clone();
        let l1 = Arc::clone(&launcher);
        let task1 = tokio::spawn(async move {
            r1.reconcile(
                DesiredCoreIntent::running(sample_descriptor("one")),
                l1.as_ref(),
            )
            .await
        });

        first_launch_entered.notified().await;

        // While rev 1 is in flight, queue rev 2 ("two") then rev 3 ("three").
        let r2 = reconciler.clone();
        let l2 = Arc::clone(&launcher);
        let rev2 = r2
            .submit(DesiredCoreIntent::running(sample_descriptor("two")))
            .unwrap();
        assert_eq!(rev2, 2);

        let rev3 = reconciler
            .submit(DesiredCoreIntent::running(sample_descriptor("three")))
            .unwrap();
        assert_eq!(rev3, 3);

        // Start a task pinned to rev 2 while rev 1 still holds the worker gate.
        let task2 =
            tokio::spawn(async move { r2.reconcile_revision(Some(rev2), l2.as_ref()).await });

        // Release rev 1: it must detect superseding, stop its stale lease, and return Superseded.
        release_first_launch.notify_one();
        let out1 = task1.await.unwrap().unwrap();
        assert_eq!(
            out1,
            ReconcileOutcome::Superseded {
                revision: 1,
                latest_revision: 3,
            }
        );

        // Rev 2 must immediately observe superseding by rev 3 and NEVER launch "two".
        let out2 = task2.await.unwrap().unwrap();
        assert_eq!(
            out2,
            ReconcileOutcome::Superseded {
                revision: 2,
                latest_revision: 3,
            }
        );

        // Worker steps once to apply latest (rev 3 = "three").
        let out3 = reconciler
            .reconcile_latest(launcher.as_ref())
            .await
            .unwrap();
        assert_eq!(out3, ReconcileOutcome::Applied { revision: 3 });

        let recorded = launched_tags.lock();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "/fake/one");
        assert!(
            !recorded[0].1.is_running(),
            "superseded lease must be stopped"
        );
        assert_eq!(recorded[1].0, "/fake/three");
        assert!(recorded[1].1.is_running(), "latest lease must stay active");
    }

    #[tokio::test]
    async fn reconciler_gates_new_running_launch_on_unreclaimed_leftover_lease() {
        let leftovers = LeftoverLeaseTracker::new();
        let reconciler = IntentReconciler::new(leftovers.clone());
        let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
        let launched_ref = Arc::clone(&launched);

        let launcher = LocalLauncher::with_blocking(leftovers.clone(), move |descriptor| {
            let lease = FakeLease::with_descriptor(true, false, Some(700), descriptor);
            launched_ref.lock().push(Arc::clone(&lease));
            Ok(lease)
        });

        // Start initial lease A.
        assert_eq!(
            reconciler
                .reconcile(
                    DesiredCoreIntent::running(sample_descriptor("a")),
                    &launcher,
                )
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 1 }
        );
        let first_lease = Arc::clone(&launched.lock()[0]);

        // Make lease A fail to confirm exit.
        first_lease.fail_stop.store(true, Ordering::Release);
        assert!(
            reconciler
                .reconcile(DesiredCoreIntent::Stopped, &launcher)
                .await
                .is_err()
        );
        assert!(leftovers.has_pending());
        assert!(reconciler.active_lease().is_none());

        // Attempt to start lease B while A is still an unreclaimed leftover: MUST fail without spawning B.
        assert!(
            reconciler
                .reconcile(
                    DesiredCoreIntent::running(sample_descriptor("b")),
                    &launcher,
                )
                .await
                .is_err()
        );
        assert_eq!(
            launched.lock().len(),
            1,
            "dual kernel launch must be blocked"
        );

        // Once leftover A confirms exit, starting B reclaims A and launches B.
        first_lease.fail_stop.store(false, Ordering::Release);
        assert_eq!(
            reconciler
                .reconcile(
                    DesiredCoreIntent::running(sample_descriptor("b")),
                    &launcher,
                )
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 4 }
        );
        assert!(!leftovers.has_pending());
        assert_eq!(launched.lock().len(), 2);
        assert!(!first_lease.is_running());
        assert!(launched.lock()[1].is_running());
    }

    #[tokio::test]
    async fn reconciler_closed_state_stops_active_lease_and_rejects_future_intents() {
        let leftovers = LeftoverLeaseTracker::new();
        let reconciler = IntentReconciler::new(leftovers.clone());
        let launched = Arc::new(Mutex::new(Vec::<Arc<FakeLease>>::new()));
        let launched_ref = Arc::clone(&launched);

        let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
            let lease = FakeLease::with_descriptor(true, false, Some(800), descriptor);
            launched_ref.lock().push(Arc::clone(&lease));
            Ok(lease)
        });

        reconciler
            .reconcile(
                DesiredCoreIntent::running(sample_descriptor("run")),
                &launcher,
            )
            .await
            .unwrap();
        assert_eq!(launched.lock().len(), 1);

        // Close stops the active lease.
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::Closed, &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Applied { revision: 2 }
        );
        assert!(reconciler.is_closed());
        assert!(!launched.lock()[0].is_running());
        assert!(reconciler.active_lease().is_none());

        // Repeat Closed coalesces.
        assert_eq!(
            reconciler
                .reconcile(DesiredCoreIntent::Closed, &launcher)
                .await
                .unwrap(),
            ReconcileOutcome::Coalesced { revision: 3 }
        );

        // Subsequent Running or Stopped intents are rejected.
        assert!(
            reconciler
                .reconcile(
                    DesiredCoreIntent::running(sample_descriptor("after-close")),
                    &launcher,
                )
                .await
                .is_err()
        );
        assert!(
            reconciler
                .reconcile(DesiredCoreIntent::Stopped, &launcher)
                .await
                .is_err()
        );
        assert_eq!(launched.lock().len(), 1);
    }

    #[tokio::test]
    async fn reconciler_batch_submit_jumps_straight_to_latest_revision() {
        let leftovers = LeftoverLeaseTracker::new();
        let reconciler = IntentReconciler::new(leftovers.clone());
        let launched_homes = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let homes_ref = Arc::clone(&launched_homes);

        let launcher = LocalLauncher::with_blocking(leftovers, move |descriptor| {
            homes_ref.lock().push(descriptor.home_dir.clone());
            Ok(FakeLease::with_descriptor(
                true,
                false,
                Some(900),
                descriptor,
            ))
        });

        assert_eq!(
            reconciler
                .submit(DesiredCoreIntent::running(sample_descriptor("r1")))
                .unwrap(),
            1
        );
        assert_eq!(
            reconciler
                .submit(DesiredCoreIntent::running(sample_descriptor("r2")))
                .unwrap(),
            2
        );
        assert_eq!(
            reconciler
                .submit(DesiredCoreIntent::running(sample_descriptor("r3")))
                .unwrap(),
            3
        );
        assert_eq!(reconciler.latest_revision(), 3);
        assert_eq!(reconciler.applied_revision(), 0);

        assert_eq!(
            reconciler.reconcile_latest(&launcher).await.unwrap(),
            ReconcileOutcome::Applied { revision: 3 }
        );
        assert_eq!(reconciler.applied_revision(), 3);
        assert_eq!(*launched_homes.lock(), vec![PathBuf::from("/fake/r3")]);

        // Reconciling latest again with no change coalesces.
        assert_eq!(
            reconciler.reconcile_latest(&launcher).await.unwrap(),
            ReconcileOutcome::Coalesced { revision: 3 }
        );
        assert_eq!(launched_homes.lock().len(), 1);
    }
}
