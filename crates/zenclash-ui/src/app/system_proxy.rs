use std::path::PathBuf;

use super::{Context, ZenClashApp};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum QuitState {
    #[default]
    Idle,
    InProgress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconcileOutcome {
    Unchanged,
    Restored,
    Released,
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
                return tokio::task::spawn_blocking(move || -> Result<ReconcileOutcome, String> {
                    let operation = controller.begin_operation();
                    let preferences = store.load().map_err(|error| error.to_string())?;
                    if !preferences.system_proxy_enabled {
                        return Ok(ReconcileOutcome::Unchanged);
                    }
                    let Some(ownership) = preferences.system_proxy_ownership else {
                        return Ok(ReconcileOutcome::Unchanged);
                    };
                    let released = operation
                        .release_if_owned(&ownership)
                        .map_err(|error| error.to_string())?;
                    store
                        .update(|preferences| preferences.system_proxy_ownership = None)
                        .map_err(|error| error.to_string())?;
                    Ok(if released {
                        ReconcileOutcome::Released
                    } else {
                        ReconcileOutcome::Unchanged
                    })
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
                return tokio::task::spawn_blocking(move || -> Result<ReconcileOutcome, String> {
                    let operation = controller.begin_operation();
                    let preferences = store.load().map_err(|error| error.to_string())?;
                    if !preferences.system_proxy_enabled {
                        return Ok(ReconcileOutcome::Unchanged);
                    }
                    if let Some(ownership) = preferences.system_proxy_ownership {
                        operation
                            .release_if_owned(&ownership)
                            .map_err(|error| error.to_string())?;
                        store
                            .update(|preferences| preferences.system_proxy_ownership = None)
                            .map_err(|error| error.to_string())?;
                    }
                    Err(zenclash_i18n::text(
                        "app.system_proxy.errors.missing_port_released",
                    ))
                })
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "app.system_proxy.errors.invalid_release_task",
                        &[("error", error.to_string())],
                    )
                })?;
            };
            tokio::task::spawn_blocking(move || -> Result<ReconcileOutcome, String> {
                let operation = controller.begin_operation();
                let preferences = store.load().map_err(|error| error.to_string())?;
                if !preferences.system_proxy_enabled {
                    return Ok(ReconcileOutcome::Unchanged);
                }
                let ownership = operation
                    .apply(
                        true,
                        preferences.system_proxy_mode,
                        &preferences.system_proxy_host,
                        port,
                        &preferences.system_proxy_bypass,
                        &preferences.system_proxy_pac_script,
                    )
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        zenclash_i18n::text("app.system_proxy.errors.ownership_missing")
                    })?;
                if let Err(error) = store.update(|preferences| {
                    preferences.system_proxy_ownership = Some(ownership.clone());
                }) {
                    let release = operation.release_if_owned(&ownership);
                    return Err(match release {
                        Ok(_) => zenclash_i18n::text_with(
                            "app.system_proxy.errors.ownership_save_released",
                            &[("error", error.to_string())],
                        ),
                        Err(release_error) => zenclash_i18n::text_with(
                            "app.system_proxy.errors.ownership_save_release_failed",
                            &[
                                ("error", error.to_string()),
                                ("release_error", release_error.to_string()),
                            ],
                        ),
                    });
                }
                Ok(ReconcileOutcome::Restored)
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
                    Ok(Ok(ReconcileOutcome::Released)) => {
                        tracing::warn!(
                            "disabled owned system proxy because the managed core is unavailable"
                        );
                    }
                    Ok(Ok(ReconcileOutcome::Restored)) => {
                        tracing::info!("reconciled owned system proxy with runtime port");
                    }
                    Ok(Ok(ReconcileOutcome::Unchanged)) => {}
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
        let process = self.mihomo_process.clone();
        let task = self.runtime.spawn(async move {
            let mut failures = Vec::new();
            if let Some(store) = store {
                let result = tokio::task::spawn_blocking(move || {
                    let operation = controller.begin_operation();
                    let preferences = store.load().map_err(|error| error.to_string())?;
                    if !preferences.system_proxy_enabled {
                        return Ok(());
                    }
                    let Some(ownership) = preferences.system_proxy_ownership else {
                        return Ok(());
                    };
                    operation
                        .release_if_owned(&ownership)
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
            if let Some(process) = process {
                let result = tokio::task::spawn_blocking(move || {
                    process.stop().map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failures.push(zenclash_i18n::text_with(
                        "app.system_proxy.errors.quit_core",
                        &[("error", error)],
                    )),
                    Err(error) => {
                        failures.push(zenclash_i18n::text_with(
                            "app.system_proxy.errors.quit_core_task",
                            &[("error", error.to_string())],
                        ));
                    }
                }
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
