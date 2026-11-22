//! Typed failures crossing the capture coordinator's backend and core seams.

pub use crate::session_errors::{TrafficCaptureError, TrafficCaptureTask};

pub(super) fn task_failure(
    task: TrafficCaptureTask,
    source: tokio::task::JoinError,
) -> TrafficCaptureError {
    TrafficCaptureError::Task { task, source }
}
