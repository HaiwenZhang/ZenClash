pub mod command;
pub use command::IpcCommand;

pub mod structure;
pub use structure::{
    AuthenticatedRequest, AuthenticatedSessionRequest, ClashConfig, CoreAvailability, CoreConfig, CoreInspection,
    CoreRequirement, InstallationStatus, MacosProxyConfig, OWNER_TOKEN_FILE_NAME, OwnerCredentials, OwnerIdentity,
    OwnerSessionHandle, OwnerSessionProof, ProtocolInfo, ProtocolVersion, ProxyApplyOutcome, RemoteProvider,
    RuntimeAsset, RuntimeBundle, RuntimeFileOutcome, RuntimeFileRequest, SERVICE_PROTOCOL_HEADER,
    SESSION_TOKEN_HEX_LEN, ServiceErrorCode, ServiceLifecycleState, ServiceStatusSnapshot, StageRejection,
    StageRuntimeOutcome, StartClashRequest, StartClashResult, WriterConfig, owner_key,
};

pub mod paths;
pub use paths::{
    CORE_DISPLACED_EXTENSION, CORE_STAGING_EXTENSION, OwnerPaths, ServicePaths, mihomo_ipc_path, service_paths,
};
#[cfg(feature = "standalone")]
pub use paths::{prepare_core_install_directory, prepare_service_install_directory};

#[cfg(feature = "standalone")]
mod atomic_file;
#[cfg(feature = "standalone")]
mod auth;
#[cfg(feature = "standalone")]
mod desired;
#[cfg(feature = "standalone")]
mod installation;
#[cfg(feature = "standalone")]
mod legacy_cleanup;
#[cfg(feature = "standalone")]
mod logger;
#[cfg(feature = "standalone")]
mod maintenance;
#[cfg(feature = "standalone")]
mod manager;
#[cfg(feature = "standalone")]
mod owner;
#[cfg(feature = "standalone")]
mod owner_lease;
#[cfg(feature = "standalone")]
mod process;
#[cfg(feature = "standalone")]
mod proxy;
#[cfg(feature = "standalone")]
mod reconcile;
#[cfg(feature = "standalone")]
mod repair;
#[cfg(feature = "standalone")]
mod runtime;
#[cfg(feature = "standalone")]
mod runtime_generation;
#[cfg(feature = "standalone")]
mod server;
#[cfg(feature = "standalone")]
mod state;
#[cfg(feature = "standalone")]
mod status;
#[cfg(feature = "test")]
mod test_credentials;
#[cfg(feature = "standalone")]
mod trusted_core_location;
#[cfg(all(feature = "standalone", unix))]
mod unix_security;
#[cfg(all(feature = "standalone", windows))]
mod windows_legacy_cleanup;
#[cfg(all(feature = "standalone", windows))]
mod windows_security;

/// Platform-specific service directory hardening.
#[cfg(all(feature = "standalone", unix))]
pub(in crate::core) use unix_security as platform_security;
#[cfg(all(feature = "standalone", windows))]
pub(in crate::core) use windows_security as platform_security;

#[cfg(feature = "standalone")]
pub use desired::{ActiveOwnerState, DesiredState, load_active_owner, load_owner_desired_state, restore_desired_state};
#[cfg(feature = "standalone")]
pub use maintenance::{cleanup_stale_owner_state, repair_active_owner_state};
#[cfg(all(feature = "standalone", feature = "test"))]
pub use manager::{CoreWatchdogTestConfig, set_core_watchdog_config_for_tests};
#[cfg(feature = "standalone")]
pub use owner::{ServiceOwnerGuard, acquire_service_owner};
#[cfg(feature = "standalone")]
pub use proxy::{
    apply_proxy, apply_proxy_or_direct, clear_proxy, clear_proxy_waiting_for_network, validate_proxy_config,
};
#[cfg(feature = "standalone")]
pub use reconcile::reconcile_service_startup;
#[cfg(feature = "standalone")]
pub use repair::{REPAIR_IN_PROGRESS_EXIT_CODE, ServiceRepairGate, acquire_service_repair_gate};
#[cfg(feature = "standalone")]
pub use server::{run_ipc_server, run_ipc_supervisor_until_shutdown, stop_ipc_server};
#[cfg(feature = "standalone")]
pub use state::{service_lifecycle_state, set_service_lifecycle_state};
#[cfg(feature = "test")]
pub use test_credentials::test_owner_credentials;
#[cfg(all(feature = "test", unix))]
pub use test_credentials::test_owner_credentials_for_uid;
#[cfg(feature = "standalone")]
pub use trusted_core_location::require_trusted_core_source;
