//! Native service IPC integration suite (feature = `"service-ipc-tests"`).
#![cfg(feature = "service-ipc-tests")]

use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

mod client;
mod lifetime;
mod session;

/// Serializes tests sharing the process-global test IPC socket and service manager state.
pub(crate) async fn ipc_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}
