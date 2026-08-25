use std::process::Command;

use super::{open_directory, ClipboardItem, Context, OutboundMode, Page, TrayCommand, ZenClashApp};

impl ZenClashApp {
    pub(super) fn handle_tray_command(&mut self, command: TrayCommand, cx: &mut Context<Self>) {
        match command {
            TrayCommand::ShowWindow => self.show_main_window(cx),
            TrayCommand::ToggleFloatingWindow => self.toggle_floating_window(cx),
            TrayCommand::SetRuleMode => self.set_mode(OutboundMode::Rule, cx),
            TrayCommand::SetGlobalMode => self.set_mode(OutboundMode::Global, cx),
            TrayCommand::SetDirectMode => self.set_mode(OutboundMode::Direct, cx),
            TrayCommand::SetSystemProxy { enabled, port } => {
                self.set_system_proxy_from_tray(enabled, port, cx);
            }
            TrayCommand::SetTun(enabled) => self.set_tun_from_tray(enabled, cx),
            TrayCommand::TestGroup {
                group,
                proxies,
                test_url,
            } => self.test_group_from_tray(group, proxies, test_url, cx),
            TrayCommand::SelectProxy { group, proxy } => {
                self.select_proxy_from_tray(group, proxy, cx);
            }
            TrayCommand::OpenProfiles => {
                self.navigate(Page::Profiles, cx);
                self.show_main_window(cx);
            }
            TrayCommand::OpenDirectory(path) => {
                if let Err(error) = open_directory(path) {
                    tracing::warn!(%error, "failed to start directory opener");
                }
            }
            TrayCommand::CopyEnvironment { port } => {
                let port = if port == 0 { 7890 } else { port };
                let url = format!("http://127.0.0.1:{port}");
                cx.write_to_clipboard(ClipboardItem::new_string(format!(
                    "export http_proxy={url} https_proxy={url} all_proxy={url} HTTP_PROXY={url} HTTPS_PROXY={url} ALL_PROXY={url}"
                )));
            }
            TrayCommand::LightMode => cx.hide(),
            TrayCommand::Restart => {
                if let Ok(executable) = std::env::current_exe() {
                    if let Err(error) = Command::new(executable).spawn() {
                        tracing::warn!(%error, "failed to restart ZenClash");
                        return;
                    }
                    cx.quit();
                }
            }
            TrayCommand::Quit => cx.quit(),
        }
    }

    fn set_system_proxy_from_tray(&mut self, enabled: bool, port: u16, cx: &mut Context<Self>) {
        if enabled && port == 0 {
            tracing::warn!("cannot enable system proxy without a Mihomo proxy port");
            self.refresh_tray_menu(cx);
            return;
        }
        if let Some((enabled, port)) = self.system_proxy_commands.submit((enabled, port)) {
            self.start_system_proxy_command(enabled, port, cx);
        }
    }

    fn start_system_proxy_command(&mut self, enabled: bool, port: u16, cx: &mut Context<Self>) {
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let manager = zenclash_core::SystemProxyManager::detect()?;
                manager.set_enabled(enabled, "127.0.0.1", port)
            })
            .await
            .map_err(|error| format!("系统代理后台任务异常结束：{error}"))?
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "system proxy tray command failed"),
                    Err(error) => tracing::warn!(%error, "system proxy tray task failed"),
                }
                if let Some((enabled, port)) = this.system_proxy_commands.complete() {
                    this.start_system_proxy_command(enabled, port, cx);
                } else {
                    this.refresh_tray_menu(cx);
                }
            });
        })
        .detach();
    }

    fn set_tun_from_tray(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(enabled) = self.tun_commands.submit(enabled) {
            self.start_tun_command(enabled, cx);
        }
    }

    fn start_tun_command(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let body = if enabled {
                serde_json::json!({"tun": {"enable": true}, "dns": {"enable": true}})
            } else {
                serde_json::json!({"tun": {"enable": false}})
            };
            client.patch_configs(&body).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "TUN tray command failed"),
                    Err(error) => tracing::warn!(%error, "TUN tray task failed"),
                }
                if let Some(enabled) = this.tun_commands.complete() {
                    this.start_tun_command(enabled, cx);
                } else {
                    this.refresh_tray_menu(cx);
                }
            });
        })
        .detach();
    }

    fn test_group_from_tray(
        &mut self,
        group: String,
        proxies: Vec<String>,
        test_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let mut tests = tokio::task::JoinSet::new();
            for proxy in proxies {
                let client = client.clone();
                let test_url = test_url.clone();
                tests.spawn(async move {
                    let _ = client.proxy_delay(&proxy, test_url.as_deref(), 5_000).await;
                });
            }
            while tests.join_next().await.is_some() {}
        });
        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |this, cx| {
                tracing::info!(group, "tray proxy group delay test completed");
                this.proxies_page
                    .update(cx, crate::pages::proxies::ProxiesPage::reload);
                this.refresh_tray_menu(cx);
            });
        })
        .detach();
    }

    fn select_proxy_from_tray(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if let Some((group, proxy)) = self.proxy_selection_commands.submit((group, proxy)) {
            self.start_proxy_selection(group, proxy, cx);
        }
    }

    fn start_proxy_selection(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client.change_proxy(&group, &proxy).await?;
            client.close_all_connections().await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "proxy selection from tray failed"),
                    Err(error) => tracing::warn!(%error, "proxy selection tray task failed"),
                }
                this.proxies_page
                    .update(cx, crate::pages::proxies::ProxiesPage::reload);
                if let Some((group, proxy)) = this.proxy_selection_commands.complete() {
                    this.start_proxy_selection(group, proxy, cx);
                } else {
                    this.refresh_tray_menu(cx);
                }
            });
        })
        .detach();
    }
}
