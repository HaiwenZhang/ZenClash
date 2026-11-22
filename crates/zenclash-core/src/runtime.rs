//! Layer 3: Managed core lifecycle, HTTP/IPC controller client, unified launchers, and `CoreSession`.
//!
//! Groups `MihomoClient`, `ControlledConfigStore`, unified `CoreProcessLauncher`/`CoreProcessLease`
//! abstractions, concrete `LocalLauncher`/`ServiceLauncher`, the latest-desired-intent
//! `IntentReconciler` and `LeftoverLeaseTracker`, core binary validation/update,
//! `ManagedCoreProcess`, native privileged service IPC/session, and `CoreSession`.

pub(crate) mod client;
pub(crate) mod controlled;
pub(crate) mod errors;
pub(crate) mod launcher;
pub(crate) mod local;
pub(crate) mod owner;
pub(crate) mod service;
pub(crate) mod session;

pub use crate::platform::core_kind::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use client::MihomoClient;
pub use controlled::{
    ControlledConfigError, ControlledConfigResult, ControlledConfigStore, ControlledConfigUpdate,
    ListenerPortFallback,
};
pub use errors::{CoreSessionError, TrafficCaptureError, TrafficCaptureTask};
pub use launcher::{
    CoreApplyKind, CoreBackupAdmission, CoreDataApplicationAdmission, CoreLaunchDescriptor,
    CoreProcessLauncher, CoreProcessLease, CoreRuntimeBackend, CoreRuntimeDescriptor,
    DesiredCoreIntent, IntentReconciler, KernelOwner, LauncherFuture, LeftoverLease,
    LeftoverLeaseTracker, LocalLauncher, ReconcileOutcome, ServiceLauncher,
};
pub use local::{
    CoreBinaryError, CoreBinaryInfo, CoreLaunchConfig, CoreProcessSnapshot, CoreUpdateError,
    CoreUpdateResult, CoreUpdateTransaction, ManagedCoreProcess, MihomoRelease, MihomoReleaseAsset,
    MihomoReleaseService, MihomoRuntimeResources, PreparedCoreUpdate, bundled_default_profile,
    bundled_recovery_profile, validate_core_binary, verify_ordinary_local_executable,
};
pub use owner::CoreTunPermissionStatus;
pub use service::{
    LocalGeoDataRecovery, ServiceRuntimeBundle, check_sidecar_available, current_process_elevated,
};
pub use session::{
    BuiltinResourceUpdateReceipt, CoreApplyOutcome, CoreCommittedProfileSnapshot,
    CoreInitializationOutcome, CoreInstallOutcome, CoreLifecyclePhase, CoreLifecycleSnapshot,
    CoreLocalRecoveryOutcome, CoreMaintenanceIntent, CoreObservation, CoreObservationIdentity,
    CoreRestoreSnapshot, CoreSession, CoreSessionSnapshot, EffectiveConfigIntent,
};
