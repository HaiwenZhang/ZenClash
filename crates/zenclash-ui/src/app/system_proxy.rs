use std::path::PathBuf;

use super::{Context, ZenClashApp};
use zenclash_core::CaptureOutcome;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum QuitState {
    #[default]
    Idle,
    InProgress,
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
        if self.quit_state == QuitState::InProgress {
            return;
        }
        self.quit_state = QuitState::InProgress;
        let capture = self.traffic_capture.clone();
        let core_session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            let mut failures = Vec::new();
            match capture.release_owned().await {
                Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => {
                    failures.push(zenclash_i18n::text_with(
                        "app.system_proxy.errors.quit_release",
                        &[("error", failure)],
                    ));
                }
                Ok(_) => {}
                Err(error) => failures.push(zenclash_i18n::text_with(
                    "app.system_proxy.errors.quit_release",
                    &[("error", error.to_string())],
                )),
            }
            if let Err(error) = core_session.shutdown().await {
                failures.push(zenclash_i18n::text_with(
                    "app.system_proxy.errors.quit_core",
                    &[("error", error.to_string())],
                ));
            }
            if failures.is_empty() {
                Ok::<(), String>(())
            } else {
                Err(failures.join("; "))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to disable system proxy before quitting");
                    }
                    Err(error) => tracing::warn!(%error, "system proxy quit workflow failed"),
                }
                if let Some(executable) = restart {
                    *this.restart_after_exit.lock() = Some(executable);
                }
                cx.quit();
            });
        })
        .detach();
    }
}
