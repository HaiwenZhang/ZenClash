//! Layer 0: Cross-platform OS infrastructure primitives with zero domain dependencies.
//!
//! Groups application data paths, bundled resources, single-instance process locks,
//! OS autostart integration, bounded non-blocking command execution, low-level
//! atomic/bounded file I/O, process privilege queries, data write coordination,
//! observation envelopes, and runtime core kind metadata.

pub(crate) mod autostart;
pub(crate) mod command;
pub(crate) mod core_kind;
pub(crate) mod data_coordinator;
pub(crate) mod fs_io;
pub(crate) mod instance_lock;
pub(crate) mod observation;
pub(crate) mod paths;
pub(crate) mod privilege;

pub use autostart::{AutostartError, AutostartManager, AutostartResult, AutostartStatus};
pub use command::{PlatformCommandError, PlatformCommandStage, PlatformCommandStream};
pub use core_kind::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use fs_io::{
    BoundedReadError, MAX_PROFILE_BYTES, atomic_write, read_bounded_profile_bytes,
    read_bounded_regular_file,
};
pub use instance_lock::{AppInstanceLock, AppInstanceLockError};
pub use observation::{
    CapabilityState, FirstRunStage, Observation, OperationalFailure, RecoveryAction,
};
pub use paths::{AppDataPaths, BundledResources, WorkspaceResources};
pub use privilege::current_process_elevated;
