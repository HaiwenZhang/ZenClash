//! Read-only aggregation of runtime process, controller, capture, path, and stream facts.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{runtime::Handle, sync::watch};

use crate::{CoreSession, LogMonitor, SystemProxySession, TrafficMonitor};

mod memory;
mod monitor;
mod types;

#[cfg(test)]
use crate::platform::observation::FirstRunStage;
pub use crate::platform::observation::{
    CapabilityState, Observation, OperationalFailure, RecoveryAction,
};
pub(crate) use monitor::observe_runtime_tun;
#[cfg(test)]
use monitor::{
    StatusRefreshSchedule, refresh_status_with_tun_reader, reset_old_generation_streams,
    same_generation,
};
#[cfg(test)]
use types::aggregate_tun_state;
pub use types::{
    CaptureStatus, ControllerCompatibility, ControllerStatus, ObservedPathRoute,
    OperationalSnapshot, PathStatus, ProcessRecoveryStatus, ProcessStatus, StreamStatus,
    StreamStatuses, TunCaptureStatus,
};

/// Receiver for changes to an [`OperationalSnapshot`].
pub type OperationalStatusStream = watch::Receiver<OperationalSnapshot>;

/// In-process owner of read-only operational facts.
pub struct OperationalStatus {
    snapshot: watch::Sender<OperationalSnapshot>,
    task: std::sync::OnceLock<tokio::task::AbortHandle>,
    wake: Arc<tokio::sync::Notify>,
    core_session: Option<CoreSession>,
}

impl Drop for OperationalStatus {
    fn drop(&mut self) {
        self.stop();
    }
}

impl OperationalStatus {
    /// Starts bounded polling over the supplied runtime owners.
    #[must_use]
    pub fn start(
        runtime: &Handle,
        core_session: CoreSession,
        system_proxy: Option<SystemProxySession>,
        traffic: Arc<TrafficMonitor>,
        logs: Arc<LogMonitor>,
    ) -> Arc<Self> {
        let initial = OperationalSnapshot::default();
        let (snapshot, _) = watch::channel(initial);
        let wake = Arc::new(tokio::sync::Notify::new());
        let status = Arc::new(Self {
            snapshot,
            task: std::sync::OnceLock::new(),
            wake: wake.clone(),
            core_session: Some(core_session.clone()),
        });
        let weak = Arc::downgrade(&status);
        let task = runtime.spawn(monitor::run_status_monitor(
            weak,
            core_session,
            system_proxy,
            traffic,
            logs,
            wake,
        ));
        let _ = status.task.set(task.abort_handle());
        status
    }

    /// Wakes the background status monitor to perform an immediate platform and TUN refresh.
    pub fn request_platform_refresh(&self) {
        self.wake.notify_one();
    }

    /// Cancels outstanding sampling immediately when its owning target is closed.
    pub fn stop(&self) {
        if let Some(task) = self.task.get() {
            task.abort();
        }
    }

    /// Returns a cheap point-in-time copy of every operational slice.
    #[must_use]
    pub fn snapshot(&self) -> OperationalSnapshot {
        self.snapshot.borrow().clone()
    }

    /// Subscribes to coherent snapshot replacements.
    #[must_use]
    pub fn subscribe(&self) -> OperationalStatusStream {
        self.snapshot.subscribe()
    }

    /// Records one explicit path-probe result for the expected core generation.
    ///
    /// Returns false when the core changed while the probe was in flight.
    pub fn record_path(
        &self,
        expected_generation: u64,
        result: Result<PathStatus, String>,
    ) -> bool {
        let mut applied = false;
        self.snapshot.send_modify(|snapshot| {
            let current_generation = match &snapshot.process {
                Observation::Fresh { value, .. } | Observation::Stale { value, .. } => {
                    Some(value.generation)
                }
                Observation::Loading => self.core_session.as_ref().map(CoreSession::generation),
                Observation::Failed { .. } => None,
            };
            if current_generation != Some(expected_generation) {
                return;
            }
            snapshot.path =
                Observation::record(&snapshot.path, result, now_ms(), RecoveryAction::Retry);
            applied = true;
        });
        applied
    }

    pub(super) fn replace(&self, snapshot: OperationalSnapshot) {
        self.snapshot.send_replace(snapshot);
    }
}

pub(super) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
