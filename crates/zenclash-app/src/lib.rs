//! Headless application orchestration layer for `ZenClash`.
//!
//! Coordinates multi-store business commands, cancellable startup and recovery,
//! outbound-mode synchronization, traffic-history sampling, edge-triggered
//! automatic monitoring, Safe Mode isolation, and safe 3-stage shutdown without
//! depending on any GUI framework.

#![deny(missing_docs)]

/// Edge-triggered network/source observation and injected-store source scanning.
pub mod automatic;
/// Cancellable multi-stage startup, configuration-layer verification, and core recovery.
pub mod bootstrap;
/// Cross-store application commands and shared recovery record.
pub mod commands;
/// Shared outbound routing modes and serialized intent coordination.
pub mod outbound_mode;
/// Safe Mode detection, isolated data-root derivation, and host side-effect guards.
pub mod safe_mode;
/// Safe 3-stage interactive and native shutdown coordination.
pub mod shutdown;
/// Background traffic-history sampling and bounded SQLite persistence.
pub mod traffic_history;

pub use automatic::{
    EdgeStableObservation, PendingSourceRevision, SourceRevisionKey, SourceScanner,
    StableObservation,
};
pub use bootstrap::{
    AppServices, BootstrapCleanup, BootstrapInputs, BootstrapOrchestrator, BootstrapOwner,
    BootstrappedApplication, RestartRequest, StartupBlocked, StartupCancellation, StartupFailure,
    StartupLayers, StartupOutcome, StartupPlatform,
};
pub use commands::{
    ApplicationCommands, BackupRecoveryReceipt, BackupRestoration, BackupRestoreReceipt,
    DeletedProfileOutcome, OverrideApplication, OverrideApplicationFailure,
    OverrideApplicationReceipt, OverrideFailureKind, ProfileFailure, ProfileReceipt,
};
pub use outbound_mode::{OutboundMode, OutboundModeCoordinator, synchronize_runtime_mode};
pub use safe_mode::SafeMode;
pub use shutdown::{QuitState, ShutdownCoordinator, ShutdownOutcome, finish_native_shutdown};
pub use traffic_history::TrafficHistorySession;
