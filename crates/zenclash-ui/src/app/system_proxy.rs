use std::{path::PathBuf, process::Command};

use super::{Context, SystemProxyMode, ZenClashApp};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum QuitState {
    #[default]
    Idle,
    InProgress,
}

impl ZenClashApp {
    pub(super) fn restore_pac_proxy(&mut self, cx: &mut Context<Self>) {
        if self.preferences.system_proxy_mode != SystemProxyMode::Pac {
            return;
        }
        let client = self.client.clone();
        let controller = self.system_proxy_controller.clone();
        let host = self.preferences.system_proxy_host.clone();
        let script = self.preferences.system_proxy_pac_script.clone();
        let bypass = self.preferences.system_proxy_bypass.clone();
        let task = self.runtime.spawn(async move {
            let status = tokio::task::spawn_blocking(|| {
                zenclash_core::SystemProxyManager::detect()?.status()
            });
            let (config, status) = tokio::join!(client.runtime_config(), status);
            let config = config.map_err(|error| error.to_string())?;
            let status = status
                .map_err(|error| format!("PAC 启动状态任务异常结束：{error}"))?
                .map_err(|error| error.to_string())?;
            if !status.auto_enabled {
                return Ok::<bool, String>(false);
            }
            let port = [config.mixed_port, config.port, config.socks_port]
                .into_iter()
                .find(|port| *port > 0)
                .ok_or_else(|| "PAC 恢复失败：当前内核没有可用代理端口".to_owned())?;
            tokio::task::spawn_blocking(move || {
                controller
                    .set_enabled(true, SystemProxyMode::Pac, &host, port, &bypass, &script)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("PAC 恢复任务异常结束：{error}"))??;
            Ok(true)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(true)) => tracing::info!("restored PAC system proxy listener"),
                    Ok(Ok(false)) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "failed to restore PAC system proxy"),
                    Err(error) => tracing::warn!(%error, "PAC restore workflow failed"),
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
        let owns_pac = controller.pac_status().is_some();
        let task = self.runtime.spawn(async move {
            if !owns_pac {
                return Ok(());
            }
            tokio::task::spawn_blocking(move || {
                controller
                    .set_enabled(false, SystemProxyMode::Pac, "", 0, &[], "")
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("退出前关闭 PAC 任务异常结束：{error}"))?
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to disable PAC before quitting");
                    }
                    Err(error) => tracing::warn!(%error, "PAC quit workflow failed"),
                }
                if let Some(executable) = restart {
                    if let Err(error) = Command::new(executable).spawn() {
                        tracing::warn!(%error, "failed to restart ZenClash");
                        this.quit_state = QuitState::Idle;
                        return;
                    }
                }
                cx.quit();
            });
        })
        .detach();
    }
}
