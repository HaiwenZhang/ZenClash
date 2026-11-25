//! Native privileged service IPC, staged runtime bundles, and authenticated service sessions.

pub(crate) mod bundle;
/// Native service sessions, controller transport, authorization and runtime state.
pub mod ipc;
pub(crate) mod session;

pub use bundle::{LocalGeoDataRecovery, ServiceRuntimeBundle};
pub use ipc::{check_sidecar_available, current_process_elevated};
