#![deny(clippy::undocumented_unsafe_blocks)]

// ZenClash application execution lifetime adaptation, 2026-10-04. GPL-3.0-only.
// Native exclusion is provided by the upstream service; app reaping is retained from ZenClash.
use std::io;

/// Retains the upstream service/sidecar exclusion through the application's child lifetime.
pub struct CoreExecutionGuard {
    _native: zenclash_service::execution::CoreExecutionGuard,
}

impl CoreExecutionGuard {
    /// Acquires the same native exclusion as service-owned core execution.
    ///
    /// # Errors
    /// Returns native lock, residual-core or service-inspection errors when execution cannot be reserved.
    pub fn acquire() -> anyhow::Result<Self> {
        Ok(Self {
            _native: zenclash_service::execution::CoreExecutionGuard::acquire()?,
        })
    }

    /// Keeps exclusion until the application confirms termination, including failed kill/wait.
    pub fn release_after_exit(self, confirm: impl FnMut() -> bool + Send + 'static) {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Some((self, confirm))));
        let worker = pending.clone();
        let started = std::thread::Builder::new()
            .name("zenclash-core-reaper".into())
            .spawn(move || {
                loop {
                    let mut state = worker.lock().unwrap_or_else(|error| error.into_inner());
                    if state.as_mut().is_none_or(|(_, confirm)| confirm()) {
                        state.take();
                        return;
                    }
                    drop(state);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });
        if started.is_err() {
            // Thread creation failure must not publish a free slot for a live child.
            std::mem::forget(pending);
        }
    }
}

pub use crate::platform::privilege::current_process_elevated;

/// Runs the upstream residual-service and core occupancy checks before sidecar fallback.
///
/// # Errors
/// Returns native inspection errors or evidence of a running service/residual core.
pub async fn check_sidecar_available() -> io::Result<()> {
    zenclash_service::execution::check_sidecar_available()
        .await
        .map_err(io::Error::other)
}
