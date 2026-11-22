use std::path::PathBuf;

use zenclash_app::shutdown::{ShutdownCoordinator, ShutdownOutcome};
use zenclash_core::{CaptureOutcome, CoreSession, TrafficCaptureSession};

use crate::shell::app::ZenClashApp;
use gpui_kit::Context;

pub(in crate::shell) use zenclash_app::shutdown::QuitState;
pub use zenclash_app::shutdown::finish_native_shutdown;

pub(in crate::shell) fn observe_native_quit(
    core: &CoreSession,
    capture: &TrafficCaptureSession,
    history: Option<&std::sync::Arc<crate::shell::app::TrafficHistorySession>>,
    runtime: &tokio::runtime::Handle,
    cx: &mut gpui_kit::App,
) {
    let coordinator =
        ShutdownCoordinator::new(core.clone(), Some(capture.clone()), history.cloned());
    let runtime = runtime.clone();
    cx.on_app_quit(move |_| {
        let coordinator = coordinator.clone();
        let task = runtime.spawn(async move {
            if let Err(error) = coordinator.execute_native().await {
                tracing::warn!(%error, "native application quit cleanup failed");
            }
        });
        async move {
            let _ = task.await;
        }
    })
    .detach();
}

impl ZenClashApp {
    pub(in crate::shell) fn restore_system_proxy(&mut self, cx: &mut Context<Self>) {
        let capture = self.services.traffic_capture.clone();
        let task = self
            .services
            .runtime
            .spawn(async move { capture.reconcile().await });
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
                        tracing::info!(
                            observed = ?outcome.snapshot().observed_plan,
                            "reconciled traffic capture state"
                        );
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

    pub(in crate::shell) fn begin_quit(
        &mut self,
        restart: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.begin_quit_mode(restart, false, cx);
    }

    pub(in crate::shell) fn begin_quit_mode(
        &mut self,
        restart: Option<PathBuf>,
        continue_local: bool,
        cx: &mut Context<Self>,
    ) {
        if self.shell.quit_state == QuitState::InProgress {
            return;
        }
        self.shell.quit_state = QuitState::InProgress;
        let coordinator = ShutdownCoordinator::new(
            self.services.core_session.clone(),
            Some(self.services.traffic_capture.clone()),
            self.services.traffic_history_session.clone(),
        );
        let preferences = self.preferences.save_task.take();
        let task = self.services.runtime.spawn(async move {
            if let Some(preferences) = preferences {
                let _ = preferences.await;
            }
            coordinator.execute_interactive().await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let error = match result {
                    Ok(ShutdownOutcome::ReadyToExit) => None,
                    Ok(
                        ShutdownOutcome::Blocked(error)
                        | ShutdownOutcome::CoreShutdownFailed(error),
                    ) => Some(error),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    tracing::warn!(
                        %error,
                        "quit cleanup failed; keeping the application and owned services alive"
                    );
                    this.shell.quit_state = QuitState::Blocked;
                    this.show_main_window(cx);
                    this.navigate(crate::foundation::actions::Page::SystemProxy, cx);
                    let message = zenclash_i18n::text_with(
                        "app.system_proxy.errors.quit_blocked",
                        &[("error", error)],
                    );
                    this.runtime_page.update(cx, |page, cx| {
                        page.report_system_proxy_reconcile_error(&message, cx);
                    });
                    return;
                }
                if let Some(executable) = restart {
                    *this.shell.restart_after_exit.lock() =
                        Some(crate::shell::app::RestartRequest {
                            executable,
                            continue_local,
                        });
                }
                cx.quit();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests;
