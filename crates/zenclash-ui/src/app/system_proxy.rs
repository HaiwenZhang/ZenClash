use std::path::PathBuf;

use super::{Context, ZenClashApp};
use zenclash_core::{CaptureOutcome, CoreSession, TrafficCaptureError, TrafficCaptureSession};

fn released_capture(result: Result<CaptureOutcome, TrafficCaptureError>) -> Result<(), String> {
    match result {
        Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
        Err(error) => Err(error.to_string()),
        Ok(_) => Ok(()),
    }
}

/// Releases owned capture, flushes history, then stops the core after native quit.
/// The event loop has already committed to exiting, so every cleanup step is attempted.
/// A missing capture is only valid before application bootstrap creates it.
///
/// # Errors
/// Returns all capture, history or core cleanup failures; callers must suppress restart.
pub async fn finish_native_shutdown(
    core: &CoreSession,
    capture: Option<&TrafficCaptureSession>,
    history: Option<&super::TrafficHistorySession>,
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

pub(super) fn observe_native_quit(
    core: &CoreSession,
    capture: &TrafficCaptureSession,
    history: Option<&std::sync::Arc<super::TrafficHistorySession>>,
    runtime: &tokio::runtime::Handle,
    cx: &mut gpui_kit::App,
) {
    let core = core.clone();
    let capture = capture.clone();
    let history = history.cloned();
    let runtime = runtime.clone();
    cx.on_app_quit(move |_| {
        let core = core.clone();
        let capture = capture.clone();
        let history = history.clone();
        let task = runtime.spawn(async move {
            if let Err(error) =
                finish_native_shutdown(&core, Some(&capture), history.as_deref()).await
            {
                tracing::warn!(%error, "native application quit cleanup failed");
            }
        });
        async move {
            let _ = task.await;
        }
    })
    .detach();
}

async fn stop_core_after_capture_release(
    core_session: &CoreSession,
    release: Result<CaptureOutcome, TrafficCaptureError>,
    history: Option<&super::TrafficHistorySession>,
) -> Result<(), String> {
    released_capture(release)?;
    if let Some(history) = history {
        history.shutdown().await?;
    }
    core_session
        .shutdown()
        .await
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum QuitState {
    #[default]
    Idle,
    InProgress,
    Blocked,
}

impl ZenClashApp {
    pub(super) fn restore_system_proxy(&mut self, cx: &mut Context<Self>) {
        let capture = self.traffic_capture.clone();
        let task = self.runtime.spawn(async move { capture.reconcile().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(CaptureOutcome::ReconcileNeeded { failure, .. })) => {
                        tracing::warn!(%failure, "traffic capture reconciliation is required");
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_system_proxy_reconcile_error(&failure, cx);
                        });
                    }
                    Ok(Ok(outcome)) => {
                        tracing::info!(observed = ?outcome.snapshot().observed_plan, "reconciled traffic capture state");
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to reconcile traffic capture");
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_system_proxy_reconcile_error(&error.to_string(), cx);
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "traffic capture reconciliation task failed");
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_system_proxy_reconcile_error(&error.to_string(), cx);
                        });
                    }
                }
                this.refresh_tray_menu(cx);
            });
        })
        .detach();
    }

    pub(super) fn begin_quit(&mut self, restart: Option<PathBuf>, cx: &mut Context<Self>) {
        self.begin_quit_mode(restart, false, cx);
    }

    pub(super) fn begin_quit_mode(
        &mut self,
        restart: Option<PathBuf>,
        continue_local: bool,
        cx: &mut Context<Self>,
    ) {
        if self.quit_state == QuitState::InProgress {
            return;
        }
        self.quit_state = QuitState::InProgress;
        self.core_session.request_shutdown();
        let capture = self.traffic_capture.clone();
        let core_session = self.core_session.clone();
        let preferences = self.preferences_save_task.take();
        let history = self.traffic_history_session.clone();
        let task = self.runtime.spawn(async move {
            if let Some(preferences) = preferences {
                let _ = preferences.await;
            }
            stop_core_after_capture_release(
                &core_session,
                capture.release_owned().await,
                history.as_deref(),
            )
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    tracing::warn!(%error, "quit cleanup failed; keeping the application and owned services alive");
                    this.quit_state = QuitState::Blocked;
                    this.show_main_window(cx);
                    this.navigate(super::Page::SystemProxy, cx);
                    let message = zenclash_i18n::text_with("app.system_proxy.errors.quit_blocked", &[("error", error)]);
                    this.runtime_page.update(cx, |page, cx| page.report_system_proxy_reconcile_error(&message, cx));
                    return;
                }
                if let Some(executable) = restart {
                    *this.restart_after_exit.lock() = Some(super::RestartRequest { executable, continue_local });
                }
                cx.quit();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{
        ControlledConfigStore, CoreKind, CoreSessionError, MihomoClient, MihomoEndpoint,
        TrafficCaptureSession,
    };

    #[tokio::test]
    async fn native_shutdown_waits_for_capture_release_before_stopping_the_core() {
        use tokio::io::AsyncWriteExt;
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
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept())
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
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(core.generation() > generation);
    }

    #[gpui_kit::test]
    fn native_quit_observer_uses_the_capture_owner_without_a_quit_action(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use tokio::io::AsyncWriteExt;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
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
        let server = runtime.spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap();
            assert!(request.into_parts().0.starts_with("GET /configs "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });
        cx.update(|cx| {
            observe_native_quit(&core, &capture, None, runtime.handle(), cx);
            cx.shutdown();
        });
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while core.generation() == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
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
            assert!(
                stop_core_after_capture_release(&core, failure, None)
                    .await
                    .is_err()
            );
            assert!(!matches!(
                core.set_mode(&store, "rule").await,
                Err(CoreSessionError::ShuttingDown)
            ));
        }
        stop_core_after_capture_release(&core, Ok(released), None)
            .await
            .unwrap();
        assert!(matches!(
            core.set_mode(&store, "rule").await,
            Err(CoreSessionError::ShuttingDown)
        ));
    }
}
