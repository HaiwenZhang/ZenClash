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
    sync::Arc,
};

use crate::{
    CoreConfigValidator, CoreKind, MihomoEndpoint, mihomo::errors::MihomoResult,
    runtime::service::ipc::NativeController,
};

mod launchers;
mod leftover;
mod reconciler;

pub use launchers::{LocalLauncher, ServiceLauncher};
pub use leftover::{LeftoverLease, LeftoverLeaseTracker};
pub use reconciler::{DesiredCoreIntent, IntentReconciler, ReconcileOutcome};

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

#[cfg(test)]
mod tests;
