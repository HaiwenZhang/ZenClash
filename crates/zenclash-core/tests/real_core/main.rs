//! Real Mihomo end-to-end acceptance suite (`#[ignore]` tests requiring `ZENCLASH_MIHOMO_BINARY`).

use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

mod automatic;
mod config_and_overrides;
mod harness;
mod lifecycle;
mod local_recovery;
mod proxies_and_rules;

/// Serializes real-kernel tests that acquire the process-wide single-core execution lock.
pub(crate) async fn real_core_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}
