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
                .map_err(|error| format!("离线恢复时关闭系统代理任务异常结束：{error}"))?;
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
                    Err("新配置没有可用的 Mixed/HTTP 端口，ZenClash 已安全关闭系统代理".into())
                })
                .await
                .map_err(|error| format!("关闭失效系统代理任务异常结束：{error}"))?;
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
                    .ok_or_else(|| "系统代理恢复成功但没有返回所有权证据".to_owned())?;
                if let Err(error) = store.update(|preferences| {
                    preferences.system_proxy_ownership = Some(ownership.clone());
                }) {
                    let release = operation.release_if_owned(&ownership);
                    return Err(match release {
                        Ok(_) => format!("保存系统代理所有权失败：{error}；系统代理已安全关闭"),
                        Err(release_error) => format!(
                            "保存系统代理所有权失败：{error}；安全关闭也失败：{release_error}"
                        ),
                    });
                }
                Ok(ReconcileOutcome::Restored)
            })
            .await
            .map_err(|error| format!("系统代理恢复任务异常结束：{error}"))?
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
                    Ok(Err(error)) => failures.push(format!("关闭系统代理失败：{error}")),
                    Err(error) => failures.push(format!("退出前关闭系统代理任务异常结束：{error}")),
                }
            }
            if let Some(process) = process {
                let result = tokio::task::spawn_blocking(move || {
                    process.stop().map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failures.push(format!("停止内核失败：{error}")),
                    Err(error) => {
                        failures.push(format!("退出前停止内核任务异常结束：{error}"));
                    }
                }
            }
            if failures.is_empty() {
                Ok::<(), String>(())
            } else {
                Err(failures.join("；"))
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
