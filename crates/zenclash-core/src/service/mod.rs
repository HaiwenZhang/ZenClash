// Fork integration adapted for ZenClash on 2026-10-04. GPL-3.0-only; see NOTICE.md.
//! Application integration copied from Clash Verge Rev and adapted to ZenClash.

mod controller;
mod execution;
mod owner_identity;
mod running_mode;
mod runtime_bundle;
mod session;

pub mod health;
pub mod maintenance;
pub mod platform;
pub mod probe;
pub mod runstate;

/// Isolated native controller fixture; never enable `service-ipc-tests` in a release build.
#[cfg(feature = "service-ipc-tests")]
pub mod test_core;

pub use running_mode::RunningMode;
pub use session::{ServiceCallError, ServiceSession};
pub use zenclash_service::{
    CoreAvailability, CoreRequirement, InstallationStatus, MacosProxyConfig, OwnerCredentials,
    OwnerIdentity, OwnerSessionProof, ProtocolInfo, ProxyApplyOutcome, RuntimeBundle,
    RuntimeFileOutcome, RuntimeFileRequest, ServiceErrorCode, ServiceStatusSnapshot,
    StageRuntimeOutcome, WriterConfig,
};

/// Loads the same native SID/UID and private owner token used by upstream.
///
/// # Errors
/// Returns native identity, application-root ownership, private-token creation or read errors.
pub fn owner_credentials(app_root: &std::path::Path) -> anyhow::Result<OwnerCredentials> {
    owner_identity::current_owner_credentials_for_root(app_root)
}

/// Returns the native account identity used for service and sidecar IPC.
///
/// # Errors
/// Returns errors querying the native Windows SID or Unix account identity.
pub fn current_owner_identity() -> anyhow::Result<OwnerIdentity> {
    owner_identity::current_owner_identity()
}

/// Collects service assets and remote-provider declarations using upstream rules.
///
/// # Errors
/// Returns config read/decode, resource path, asset or provider-conflict errors.
pub async fn collect_runtime_bundle(
    config: &std::path::Path,
    core: &std::path::Path,
) -> anyhow::Result<RuntimeBundle> {
    runtime_bundle::collect_runtime_bundle(config, core).await
}

/// Allocates distinct provider caches when URLs share a destination.
///
/// # Errors
/// Returns invalid provider or path errors while resolving overlapping cache destinations.
pub fn resolve_provider_path_conflicts(
    config: &mut serde_yaml::Mapping,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    runtime_bundle::resolve_provider_path_conflicts(config, root)
}

/// Refuses a second core while a service or residual core owns execution.
///
/// # Errors
/// Returns native lock, residual-core or service-inspection errors when execution cannot be reserved.
pub async fn reserve_sidecar() -> anyhow::Result<zenclash_service::execution::CoreExecutionGuard> {
    zenclash_service::execution::reserve_sidecar().await
}

/// Uses the same platform owner-only pipe ACL as upstream.
#[cfg(windows)]
///
/// # Errors
/// Returns native account SID or pipe-ACL construction errors.
pub fn current_user_pipe_sddl() -> anyhow::Result<String> {
    owner_identity::current_user_pipe_sddl()
}

/// Creates a native private file for provider-cache publication by the current user.
#[cfg(windows)]
///
/// # Errors
/// Returns native file, ACL or path-safety errors; existing files are not replaced.
pub fn create_private_current_user_file(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    owner_identity::create_private_current_user_file(path)
}

/// Opens a native private file, refusing reparse points and directories.
#[cfg(windows)]
///
/// # Errors
/// Returns native file, ACL or path-safety errors for reparse points and non-files.
pub fn open_private_current_user_file(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    owner_identity::open_private_current_user_file(path)
}

/// Opens or creates a private file without weakening an existing file's ACL.
#[cfg(windows)]
///
/// # Errors
/// Returns native file, ACL or path-safety errors without weakening an existing ACL.
pub fn open_or_create_private_current_user_file(
    path: &std::path::Path,
) -> anyhow::Result<std::fs::File> {
    owner_identity::open_or_create_private_current_user_file(path)
}

/// Explicitly repairs an application-data root that belongs to another account.
#[cfg(windows)]
///
/// # Errors
/// Returns ownership inspection, administrator authorization or native ACL repair errors.
pub fn repair_app_data_root_owner(path: &std::path::Path) -> anyhow::Result<()> {
    owner_identity::repair_app_data_root_owner(path)
}

pub use runtime_bundle::RemoteProviderRef;

/// Describes remote caches with their config section/name and normalized service destination.
///
/// # Errors
/// Returns invalid provider declarations, URLs or unsafe destination-path errors.
pub fn remote_providers_of(
    config: &serde_yaml::Mapping,
    root: &std::path::Path,
) -> anyhow::Result<Vec<RemoteProviderRef>> {
    runtime_bundle::remote_providers_of(config, root)
}

/// Retains upstream service intent independently of observed health and core runtime ownership.
#[derive(Default)]
pub struct ServiceRunState {
    stored: parking_lot::Mutex<health::StoredService>,
}

impl ServiceRunState {
    /// Records the latest observed service health.
    pub fn observe(&self, health: health::ServiceHealth) {
        self.stored.lock().observe(health);
    }
    /// Retains an explicit service maintenance request.
    pub fn request(&self, action: health::PendingAction) {
        self.stored.lock().request(action);
    }
    /// Accepts local sidecar execution for this application session.
    pub fn allow_sidecar(&self) {
        self.stored.lock().allow_sidecar();
    }

    /// Projects stored intent together with current execution and privilege facts.
    pub fn snapshot(
        &self,
        mode: RunningMode,
        is_admin: bool,
        op_in_flight: bool,
    ) -> health::RunState {
        let stored = self.stored.lock();
        health::RunState {
            health: stored.health.clone(),
            pending: stored.pending,
            sidecar_allowed: stored.sidecar_allowed,
            mode,
            is_admin,
            op_in_flight,
        }
    }
}

pub use execution::{CoreExecutionGuard, check_sidecar_available, current_process_elevated};

pub use controller::{NativeController, NativeHttpError, NativeHttpResponse, NativeSocket};
