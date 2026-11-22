//! Safe 3-stage application shutdown coordination.
//!
//! Interactive quit strictly follows three stages so a failed Stage 1 never
//! poisons `CoreSession`:
//! 1. Release owned traffic capture and flush pending traffic history.
//! 2. Only after Stage 1 succeeds, call `CoreSession::request_shutdown()` and
//!    execute `CoreSession::shutdown()`.
//! 3. Report [`ShutdownOutcome::ReadyToExit`] so the UI shell exits cleanly, or
//!    [`ShutdownOutcome::Blocked`] while keeping `CoreSession` healthy.

use std::sync::Arc;

use zenclash_core::{CaptureOutcome, CoreSession, TrafficCaptureError, TrafficCaptureSession};

use crate::TrafficHistorySession;

/// Explicit lifecycle state of an interactive application shutdown attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuitState {
    /// Normal running state; no shutdown is in progress.
    #[default]
    Idle,
    /// Stage 1 or Stage 2 shutdown cleanup is currently running.
    InProgress,
    /// Stage 1 failed before poisoning `CoreSession`; the user can retry or
    /// continue using the healthy application.
    Blocked,
}

/// Result of a coordinated 3-stage interactive shutdown attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Stage 1 (capture release + history flush) and Stage 2 (`CoreSession`
    /// shutdown) succeeded; the caller may now terminate the event loop.
    ReadyToExit,
    /// Stage 1 failed before touching `CoreSession`; the session remains healthy
    /// and interactive, and the caller should enter [`QuitState::Blocked`].
    Blocked(String),
    /// Stage 2 (`CoreSession::shutdown`) failed after Stage 1 succeeded.
    CoreShutdownFailed(String),
}

/// Coordinates safe 3-stage interactive shutdown and best-effort native shutdown.
#[derive(Clone)]
pub struct ShutdownCoordinator {
    core: CoreSession,
    capture: Option<TrafficCaptureSession>,
    history: Option<Arc<TrafficHistorySession>>,
}

impl ShutdownCoordinator {
    /// Creates a shutdown coordinator for the given session, capture owner, and
    /// optional traffic-history sampler.
    #[must_use]
    pub fn new(
        core: CoreSession,
        capture: Option<TrafficCaptureSession>,
        history: Option<Arc<TrafficHistorySession>>,
    ) -> Self {
        Self {
            core,
            capture,
            history,
        }
    }

    /// Executes the safe 3-stage interactive shutdown sequence.
    ///
    /// `CoreSession::request_shutdown()` is ONLY called after owned traffic
    /// capture is cleanly released and traffic history has flushed.
    pub async fn execute_interactive(&self) -> ShutdownOutcome {
        if let Some(capture) = &self.capture
            && let Err(error) = released_capture(capture.release_owned().await)
        {
            return ShutdownOutcome::Blocked(error);
        }
        self.finish_stage_one_and_stop_core().await
    }

    /// Executes the interactive shutdown sequence given an already-completed
    /// traffic-capture release outcome.
    pub async fn execute_interactive_with_release(
        &self,
        release: Result<CaptureOutcome, TrafficCaptureError>,
    ) -> ShutdownOutcome {
        if let Err(error) = released_capture(release) {
            return ShutdownOutcome::Blocked(error);
        }
        self.finish_stage_one_and_stop_core().await
    }

    /// Executes best-effort native shutdown after the event loop has already
    /// committed to exiting.
    ///
    /// # Errors
    ///
    /// Returns all capture, history, or core cleanup failures joined together.
    pub async fn execute_native(&self) -> Result<(), String> {
        finish_native_shutdown(&self.core, self.capture.as_ref(), self.history.as_deref()).await
    }

    async fn finish_stage_one_and_stop_core(&self) -> ShutdownOutcome {
        if let Some(history) = &self.history
            && let Err(error) = history.shutdown().await
        {
            return ShutdownOutcome::Blocked(error);
        }
        self.core.request_shutdown();
        match self.core.shutdown().await {
            Ok(()) => ShutdownOutcome::ReadyToExit,
            Err(error) => ShutdownOutcome::CoreShutdownFailed(error.to_string()),
        }
    }
}

/// Releases owned capture, flushes history, then stops the core after native quit.
///
/// The event loop has already committed to exiting, so every cleanup step is
/// attempted. A missing capture is only valid before application bootstrap
/// creates it.
///
/// # Errors
///
/// Returns all capture, history, or core cleanup failures; callers must suppress
/// process restart when an error is returned.
pub async fn finish_native_shutdown(
    core: &CoreSession,
    capture: Option<&TrafficCaptureSession>,
    history: Option<&TrafficHistorySession>,
) -> Result<(), String> {
    core.request_shutdown();
    let mut failures = Vec::new();
    if let Some(capture) = capture
        && let Err(error) = released_capture(capture.release_owned().await)
    {
        failures.push(error);
    }
    if let Some(history) = history
        && let Err(error) = history.shutdown().await
    {
        failures.push(error);
    }
    if let Err(error) = core.shutdown().await {
        failures.push(error.to_string());
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn released_capture(result: Result<CaptureOutcome, TrafficCaptureError>) -> Result<(), String> {
    match result {
        Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
        Err(error) => Err(error.to_string()),
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests;
