use std::path::PathBuf;

use super::{Context, ZenClashApp};
use zenclash_core::{SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum QuitState {
    #[default]
    Idle,
    InProgress,
}

impl ZenClashApp {
    pub(super) fn restore_system_proxy(&mut self, cx: &mut Context<Self>) {
        if !self.preferences.system_proxy_enabled {
            return;
        }
        let Some(store) = self.preferences_store.clone() else {
            tracing::warn!("cannot reconcile system proxy without persistent ownership state");
            return;
        };
        let core_unavailable = self.client.endpoint().controller == "127.0.0.1:0"
            || self
                .mihomo_process
                .as_ref()
                .is_some_and(|process| !process.is_running());
        let client = self.client.clone();
        let controller = self.system_proxy_controller.clone();
        let task = self.runtime.spawn(async move {
            if core_unavailable {
                return tokio::task::spawn_blocking(move || {
                    SystemProxySession::new(store, controller)
                        .reconcile(false, None)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "app.system_proxy.errors.offline_release_task",
                        &[("error", error.to_string())],
                    )
                })?;
            }
            let config = client
                .runtime_config()
                .await
                .map_err(|error| error.to_string())?;
            let Some(port) = config.system_proxy_port() else {
                return tokio::task::spawn_blocking(move || {
                    SystemProxySession::new(store, controller)
                        .reconcile(true, None)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "app.system_proxy.errors.invalid_release_task",
                        &[("error", error.to_string())],
                    )
                })?;
            };
            tokio::task::spawn_blocking(move || {
                SystemProxySession::new(store, controller)
                    .reconcile(true, Some(port))
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "app.system_proxy.errors.restore_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(SystemProxyReconcileOutcome::Released {
                        reason: SystemProxyReleaseReason::CoreUnavailable,
                        native_matched,
                    })) => {
                        tracing::warn!(native_matched, "released owned system proxy because the core is unavailable");
                    }
                    Ok(Ok(SystemProxyReconcileOutcome::Released {
                        reason: SystemProxyReleaseReason::MissingPort,
                        native_matched,
                    })) => {
                        let error = zenclash_i18n::text("app.system_proxy.errors.missing_port_released");
                        tracing::warn!(native_matched, %error, "released owned system proxy without a runtime port");
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_system_proxy_reconcile_error(&error, cx);
                        });
                    }
                    Ok(Ok(SystemProxyReconcileOutcome::Restored)) => {
                        tracing::info!("reconciled owned system proxy with runtime port");
                    }
                    Ok(Ok(SystemProxyReconcileOutcome::Unchanged)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to reconcile owned system proxy");
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_system_proxy_reconcile_error(&error, cx);
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "system proxy reconciliation workflow failed");
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
        let controller = self.system_proxy_controller.clone();
        let store = self.preferences_store.clone();
        let core_session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            let mut failures = Vec::new();
            if let Some(store) = store {
                let result = tokio::task::spawn_blocking(move || {
                    SystemProxySession::new(store, controller)
                        .release_owned()
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failures.push(zenclash_i18n::text_with(
                        "app.system_proxy.errors.quit_release",
                        &[("error", error)],
                    )),
                    Err(error) => failures.push(zenclash_i18n::text_with(
                        "app.system_proxy.errors.quit_release_task",
                        &[("error", error.to_string())],
                    )),
                }
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
