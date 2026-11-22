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
mod tests {
    use super::*;
    use zenclash_core::{
        ControlledConfigStore, CoreKind, CoreSessionError, MihomoClient, MihomoEndpoint,
        TrafficCaptureSession,
    };

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

    #[gpui_kit::test]
    fn begin_quit_mode_keeps_core_session_healthy_when_capture_release_fails(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::{AppContext as _, component::Root, px, size};
        use std::time::Duration;
        use zenclash_core::{
            AppPreferencesStore, CoreLifecyclePhase, SystemProxyController, SystemProxySession,
        };

        let root = std::env::temp_dir().join(format!(
            "zenclash-begin-quit-blocked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let prefs_path = root.join("preferences.json");
        std::fs::write(&prefs_path, b"{invalid").unwrap();

        let fixture = crate::test_support::RuntimePageFixture::new();
        let mut services = crate::test_support::shell_tray::app_test_services(&fixture);
        let core = services.core_session.clone();
        let controlled = services.controlled_config_store.clone();
        let runtime = services.runtime.clone();
        let system_proxy = SystemProxySession::new(
            AppPreferencesStore::new(prefs_path),
            SystemProxyController::default(),
        );
        services.traffic_capture = TrafficCaptureSession::new(
            core.clone(),
            controlled.clone(),
            Some(system_proxy.clone()),
            None,
        );
        services.system_proxy_session = Some(system_proxy);

        cx.executor().allow_parking();
        cx.update(crate::shell::app::init);
        let mut owner = None;
        let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
            let preferences = services.preferences.clone();
            let app =
                cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
            owner = Some(app.clone());
            Root::new(app, window, cx)
        });
        let owner = owner.unwrap();

        cx.update(|cx| {
            owner.update(cx, |app, cx| app.begin_quit_mode(None, false, cx));
        });
        cx.foreground_executor().clone().block_test(async {
            for _ in 0..1_000 {
                if cx.update(|cx| owner.read(cx).shell.quit_state == QuitState::Blocked) {
                    return;
                }
                runtime
                    .spawn(async {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    })
                    .await
                    .unwrap();
            }
            panic!("begin_quit_mode did not settle into QuitState::Blocked");
        });

        assert!(!core.is_shutting_down());
        assert_ne!(
            core.lifecycle_snapshot().phase,
            CoreLifecyclePhase::ShuttingDown
        );
        cx.foreground_executor().clone().block_test(async {
            runtime
                .spawn(async move {
                    let admission = core.begin_data_application().await;
                    assert!(
                        admission.is_ok(),
                        "blocked quit must keep data application admission open"
                    );
                    drop(admission);
                    assert!(!matches!(
                        core.set_mode(&controlled, "rule").await,
                        Err(CoreSessionError::ShuttingDown)
                    ));
                })
                .await
                .unwrap();
        });

        cx.update_window(main.into(), |_, window, _| window.remove_window())
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
