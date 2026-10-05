// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
mod channel;
mod core;
#[cfg(any(feature = "client", feature = "standalone"))]
pub mod execution;
#[cfg(any(feature = "client", feature = "standalone"))]
pub mod management;

#[cfg(feature = "client")]
mod client;

pub use channel::{
    CHANNEL_IDENTITY, ChannelIdentity, MACOS_APP_BUNDLE_ID, MACOS_SERVICE_ID, SERVICE_DISPLAY_NAME, SERVICE_SLUG,
    WINDOWS_SERVICE_NAME,
};
pub use core::{
    AuthenticatedRequest, AuthenticatedSessionRequest, ClashConfig, CoreAvailability, CoreConfig, CoreInspection,
    CoreRequirement, InstallationStatus, IpcCommand, MacosProxyConfig, OWNER_TOKEN_FILE_NAME, OwnerCredentials,
    OwnerIdentity, OwnerSessionHandle, OwnerSessionProof, ProtocolInfo, ProtocolVersion, ProxyApplyOutcome,
    RemoteProvider, RuntimeAsset, RuntimeBundle, RuntimeFileOutcome, RuntimeFileRequest, SERVICE_PROTOCOL_HEADER,
    SESSION_TOKEN_HEX_LEN, ServiceErrorCode, ServiceLifecycleState, ServiceStatusSnapshot, StageRejection,
    StageRuntimeOutcome, StartClashRequest, StartClashResult, WriterConfig, mihomo_ipc_path, owner_key,
};
pub use core::{CORE_DISPLACED_EXTENSION, CORE_STAGING_EXTENSION, OwnerPaths, ServicePaths, service_paths};

#[cfg(feature = "standalone")]
pub use core::{
    ActiveOwnerState, DesiredState, REPAIR_IN_PROGRESS_EXIT_CODE, ServiceOwnerGuard, ServiceRepairGate,
    acquire_service_owner, acquire_service_repair_gate, cleanup_stale_owner_state, load_active_owner,
    load_owner_desired_state, prepare_core_install_directory, prepare_service_install_directory,
    reconcile_service_startup, repair_active_owner_state, require_trusted_core_source, restore_desired_state,
    run_ipc_server, run_ipc_supervisor_until_shutdown, service_lifecycle_state, set_service_lifecycle_state,
    stop_ipc_server,
};

#[cfg(feature = "test")]
pub use core::test_owner_credentials;
#[cfg(all(feature = "test", unix))]
pub use core::test_owner_credentials_for_uid;
#[cfg(all(feature = "standalone", feature = "test"))]
pub use core::{CoreWatchdogTestConfig, set_core_watchdog_config_for_tests};

#[cfg(feature = "client")]
pub use client::*;

#[cfg(all(target_os = "macos", not(feature = "test"), not(feature = "development-channel")))]
pub static IPC_PATH: &str = "/var/run/zenclash-service/service.sock";
#[cfg(all(target_os = "macos", not(feature = "test"), feature = "development-channel"))]
pub static IPC_PATH: &str = "/var/run/zenclash-service-dev/service.sock";
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(feature = "test"),
    not(feature = "development-channel")
))]
pub static IPC_PATH: &str = "/run/zenclash-service/service.sock";
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(feature = "test"),
    feature = "development-channel"
))]
pub static IPC_PATH: &str = "/run/zenclash-service-dev/service.sock";
#[cfg(all(windows, not(feature = "test"), not(feature = "development-channel")))]
pub static IPC_PATH: &str = r"\\.\pipe\zenclash-service";
#[cfg(all(windows, not(feature = "test"), feature = "development-channel"))]
pub static IPC_PATH: &str = r"\\.\pipe\zenclash-service-dev";

#[cfg(all(feature = "test", unix))]
pub static IPC_PATH: &str = "/tmp/zenclash-service-ipc-test/service.sock";
#[cfg(all(feature = "test", windows))]
pub static IPC_PATH: &str = r"\\.\pipe\zenclash-service-test";

#[cfg(any(feature = "standalone", feature = "client"))]
pub static IPC_AUTH_EXPECT: &str =
    r#"A thing of beauty is a joy for ever. Its loveliness increases; it will never pass into nothingness."#;

pub static VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_EPOCH: u16 = 2;
pub const PROTOCOL_REVISION: u16 = 5;
pub const MIN_SUPPORTED_CLIENT_REVISION: u16 = 5;
pub const MIN_REQUIRED_SERVICE_REVISION: u16 = 5;
/// Revision that introduced `/clash/stage-runtime`.
/// This is a capability gate, not the minimum compatible service revision.
pub const MIN_SERVICE_REVISION_FOR_RUNTIME_STAGING: u16 = 2;
/// Capability revision for `/clash/runtime-file`.
pub const MIN_SERVICE_REVISION_FOR_RUNTIME_FILE_READ: u16 = 3;
