use std::path::PathBuf;

use super::{AppContext, Context, ZenClashApp};
use zenclash_core::{CaptureOutcome, CoreSession, TrafficCaptureError};

async fn stop_core_after_capture_release(
    core_session: &CoreSession,
    release: Result<CaptureOutcome, TrafficCaptureError>,
    history: Option<&super::TrafficHistorySession>,
) -> Result<(), String> {
    match release {
        Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => return Err(failure),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
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
        if let Some(panel) = self.status_panel.take() {
            let _ = cx.update_window(panel, |_, window, _| window.remove_window());
        }
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
            Err(TrafficCaptureError::Backend("release task failed".into())),
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
