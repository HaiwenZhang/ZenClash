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
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use tokio::io::AsyncWriteExt;
    use zenclash_core::{
        AppPreferences, ControlledConfigStore, CoreKind, CoreSessionError, MihomoClient,
        MihomoEndpoint, TrafficHistoryStore,
    };

    use super::*;

    #[tokio::test]
    async fn native_shutdown_waits_for_capture_release_before_stopping_the_core() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new(
                format!("http://{}", listener.local_addr().unwrap()),
                "",
            ))
            .unwrap(),
        )
        .unwrap();
        let capture = TrafficCaptureSession::new(
            core.clone(),
            ControlledConfigStore::new(std::env::temp_dir()),
            None,
            None,
        );
        let stopping = core.clone();
        let generation = core.generation();
        let shutdown =
            tokio::spawn(
                async move { finish_native_shutdown(&stopping, Some(&capture), None).await },
            );
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap();
        assert!(request.into_parts().0.starts_with("GET /configs "));
        assert!(!shutdown.is_finished());
        assert_eq!(
            core.generation(),
            generation,
            "the core must wait for capture readback"
        );
        let body = r#"{"tun":{"enable":false}}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(core.generation() > generation);
    }

    #[tokio::test]
    async fn failed_capture_release_prevents_core_shutdown_and_success_allows_it() {
        let core = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
        )
        .unwrap();
        let store = ControlledConfigStore::new(std::env::temp_dir());
        let capture = TrafficCaptureSession::new(core.clone(), store.clone(), None, None);
        let released = capture.release_owned().await.unwrap();
        let coordinator = ShutdownCoordinator::new(core.clone(), Some(capture), None);
        let partial = CaptureOutcome::ReconcileNeeded {
            plan: None,
            snapshot: released.snapshot().clone(),
            failure: "native permission rejected".into(),
        };
        for failure in [
            Ok(partial),
            Err(TrafficCaptureError::Controller(
                zenclash_core::MihomoError::Api {
                    status: 503,
                    message: "release task failed".into(),
                },
            )),
        ] {
            let outcome = coordinator.execute_interactive_with_release(failure).await;
            assert!(matches!(outcome, ShutdownOutcome::Blocked(_)));
            assert!(!core.is_shutting_down());
            assert!(!matches!(
                core.set_mode(&store, "rule").await,
                Err(CoreSessionError::ShuttingDown)
            ));
        }
        assert_eq!(
            coordinator
                .execute_interactive_with_release(Ok(released))
                .await,
            ShutdownOutcome::ReadyToExit
        );
        assert!(matches!(
            core.set_mode(&store, "rule").await,
            Err(CoreSessionError::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn failed_history_flush_blocks_shutdown_without_poisoning_core_session() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-shutdown-history-blocked-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = root.join("traffic.sqlite3");
        // Create a directory at the SQLite file path so final flush fails deterministically.
        std::fs::create_dir_all(&db_path).unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
        let core = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
        let store = TrafficHistoryStore::new(&db_path);
        let history = TrafficHistorySession::start(
            &tokio::runtime::Handle::current(),
            client,
            store,
            &AppPreferences::default(),
        );
        let coordinator = ShutdownCoordinator::new(core.clone(), None, Some(history));
        let outcome = coordinator.execute_interactive().await;
        assert!(matches!(outcome, ShutdownOutcome::Blocked(_)));
        assert!(
            !core.is_shutting_down(),
            "Stage 1 history failure must not poison CoreSession"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
