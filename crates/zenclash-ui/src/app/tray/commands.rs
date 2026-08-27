use futures_util::{stream, StreamExt};
use zenclash_core::YamlOverrideStore;

use super::{
    open_directory, ClipboardItem, Context, EnvironmentShell, OutboundMode, Page, TrayCommand,
    ZenClashApp,
};

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
            TrayCommand::SelectProfile { id } => self.select_profile_from_tray(id, cx),
            TrayCommand::OpenProfiles => {
                self.navigate(Page::Profiles, cx);
                self.show_main_window(cx);
            }
            TrayCommand::OpenDirectory(path) => {
                if let Err(error) = open_directory(path) {
                    tracing::warn!(%error, "failed to start directory opener");
                }
            }
            TrayCommand::CopyEnvironment { port, shell } => {
                if let Some(environment) = proxy_environment(shell, port) {
                    cx.write_to_clipboard(ClipboardItem::new_string(environment));
                } else {
                    tracing::warn!("cannot copy proxy environment without a live HTTP/Mixed port");
                }
            }
            TrayCommand::LightMode => cx.hide(),
            TrayCommand::Restart => {
                let executable = match std::env::current_exe() {
                    Ok(executable) => executable,
                    Err(error) => {
                        tracing::warn!(%error, "failed to resolve ZenClash executable for restart");
                        return;
                    }
                };
                if let Some(process) = &self.mihomo_process {
                    if let Err(error) = process.stop() {
                        tracing::warn!(%error, "failed to stop the core before restarting ZenClash");
                        return;
                    }
                }
                self.begin_quit(Some(executable), cx);
            }
            TrayCommand::Quit => self.begin_quit(None, cx),
        }
    }

    fn set_system_proxy_from_tray(&mut self, enabled: bool, port: u16, cx: &mut Context<Self>) {
        if enabled && port == 0 {
            tracing::warn!("cannot enable system proxy without a core proxy port");
            self.refresh_tray_menu(cx);
            return;
        }
        if let Some((enabled, port)) = self.system_proxy_commands.submit((enabled, port)) {
            self.start_system_proxy_command(enabled, port, cx);
        }
    }

    fn start_system_proxy_command(&mut self, enabled: bool, port: u16, cx: &mut Context<Self>) {
        let controller = self.system_proxy_controller.clone();
        let store = self.preferences_store.clone();
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let store =
                    store.ok_or_else(|| "应用设置存储不可用；无法记录系统代理所有权".to_owned())?;
                let operation = controller.begin_operation();
                let expected = store.load().map_err(|error| error.to_string())?;
                let ownership = if enabled {
                    Some(
                        operation
                            .apply(
                                true,
                                expected.system_proxy_mode,
                                &expected.system_proxy_host,
                                port,
                                &expected.system_proxy_bypass,
                                &expected.system_proxy_pac_script,
                            )
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| {
                                "系统代理启用成功但没有返回所有权证据".to_owned()
                            })?,
                    )
                } else {
                    if let Some(ownership) = &expected.system_proxy_ownership {
                        operation
                            .release_if_owned(ownership)
                            .map_err(|error| error.to_string())?;
                    } else {
                        operation
                            .set_enabled(
                                false,
                                expected.system_proxy_mode,
                                "",
                                0,
                                &[],
                                "",
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    None
                };
                match store.update(|preferences| {
                    preferences.system_proxy_enabled = enabled;
                    preferences.system_proxy_ownership.clone_from(&ownership);
                }) {
                    Ok(preferences) => Ok(preferences),
                    Err(error) => {
                        let rollback = operation.apply(
                            expected.system_proxy_enabled,
                            expected.system_proxy_mode,
                            &expected.system_proxy_host,
                            port,
                            &expected.system_proxy_bypass,
                            &expected.system_proxy_pac_script,
                        );
                        match rollback {
                            Ok(restored_ownership) => {
                                if restored_ownership != expected.system_proxy_ownership {
                                    if let Some(restored_ownership) = restored_ownership {
                                        if let Err(ownership_error) = store.update(|preferences| {
                                            preferences.system_proxy_ownership =
                                                Some(restored_ownership.clone());
                                        }) {
                                            let release =
                                                operation.release_if_owned(&restored_ownership);
                                            return Err(match release {
                                                Ok(_) => format!(
                                                    "保存系统代理开关失败：{error}；恢复所有权记录也失败：{ownership_error}，系统代理已安全关闭"
                                                ),
                                                Err(release_error) => format!(
                                                    "保存系统代理开关失败：{error}；恢复所有权记录失败：{ownership_error}；安全关闭也失败：{release_error}"
                                                ),
                                            });
                                        }
                                    }
                                }
                                Err(format!("保存系统代理开关失败：{error}；原状态已恢复"))
                            }
                            Err(rollback) => Err(format!(
                                "保存系统代理开关失败：{error}；恢复原状态也失败：{rollback}"
                            )),
                        }
                    }
                }
            })
            .await
            .map_err(|error| format!("系统代理后台任务异常结束：{error}"))?
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(preferences)) => {
                        this.preferences = preferences.clone();
                        this.runtime_page.update(cx, |page, cx| {
                            page.preferences_restored_from_app(preferences, cx);
                        });
                    }
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
        let controlled = self.controlled_config_store.clone();
        let profile = self.profile_path.clone();
        let uses_restart = !self.core_kind.capabilities().full_config_reload;
        let process = self.mihomo_process.clone();
        let task = self.runtime.spawn(async move {
            let Some(profile) = profile else {
                return Err("未配置当前配置文件路径".to_owned());
            };
            let body = if enabled {
                serde_json::json!({"tun": {"enable": true}, "dns": {"enable": true}})
            } else {
                serde_json::json!({"tun": {"enable": false}})
            };
            let overrides =
                tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
                    .await
                    .map_err(|error| format!("读取 YAML 覆写任务异常结束：{error}"))?
                    .map_err(|error| error.to_string())?;
            if uses_restart {
                let process = process.ok_or_else(|| {
                    "外部实验内核不能可靠应用完整 TUN 配置；请改用 ZenClash 托管内核".to_owned()
                })?;
                controlled
                    .apply_json_update_with_restart(process, profile, &body, overrides)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                controlled
                    .apply_json_update_with_overrides(&client, profile, &body, overrides)
                    .await
                    .map_err(|error| error.to_string())
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {
                        this.runtime_page.update(cx, |runtime_page, cx| {
                            runtime_page.reload_controlled_config(cx);
                        });
                    }
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
            stream::iter(proxies)
                .for_each_concurrent(Some(16), |proxy| {
                    let client = client.clone();
                    let test_url = test_url.clone();
                    async move {
                        let _ = client.proxy_delay(&proxy, test_url.as_deref(), 5_000).await;
                    }
                })
                .await;
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

    fn select_profile_from_tray(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(id) = self.profile_selection_commands.submit(id) {
            self.start_profile_selection(id, cx);
        }
    }

    fn start_profile_selection(&mut self, id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime = crate::pages::runtime::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.mihomo_process.clone(),
        );
        let task = self.runtime.spawn(async move {
            let store =
                zenclash_core::ProfileStore::discover().map_err(|error| error.to_string())?;
            crate::pages::runtime::profiles::workflow::activate_existing(
                store,
                controlled,
                core_runtime,
                id,
            )
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("状态栏配置任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(outcome) => {
                        this.runtime_page.update(cx, |runtime_page, cx| {
                            runtime_page.profile_activated_from_tray(
                                outcome.path,
                                &outcome.name,
                                cx,
                            );
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "profile selection from tray failed");
                        this.runtime_page.update(cx, |runtime_page, cx| {
                            runtime_page.report_tray_profile_error(&error, cx);
                        });
                    }
                }
                if let Some(id) = this.profile_selection_commands.complete() {
                    this.start_profile_selection(id, cx);
                } else {
                    this.refresh_tray_menu(cx);
                }
            });
        })
        .detach();
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

fn proxy_environment(shell: EnvironmentShell, port: u16) -> Option<String> {
    if port == 0 {
        return None;
    }
    let url = format!("http://127.0.0.1:{port}");
    Some(match shell {
        EnvironmentShell::Bash => {
            format!("export https_proxy={url} http_proxy={url} all_proxy={url}")
        }
        EnvironmentShell::CommandPrompt => {
            format!("set http_proxy={url}\r\nset https_proxy={url}")
        }
        EnvironmentShell::PowerShell => {
            format!(r#"$env:HTTP_PROXY="{url}"; $env:HTTPS_PROXY="{url}""#)
        }
        EnvironmentShell::Fish => {
            format!("set -x http_proxy {url}; set -x https_proxy {url}; set -x all_proxy {url}")
        }
        EnvironmentShell::Nushell => format!(
            r#"$env.HTTP_PROXY = "{url}"; $env.HTTPS_PROXY = "{url}"; $env.ALL_PROXY = "{url}""#
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{proxy_environment, EnvironmentShell};

    #[test]
    fn formats_proxy_environment_for_each_supported_shell() {
        assert_eq!(
            proxy_environment(EnvironmentShell::Bash, 7897).as_deref(),
            Some("export https_proxy=http://127.0.0.1:7897 http_proxy=http://127.0.0.1:7897 all_proxy=http://127.0.0.1:7897")
        );
        assert!(proxy_environment(EnvironmentShell::CommandPrompt, 7897)
            .is_some_and(|value| value.contains("\r\nset")));
        assert!(proxy_environment(EnvironmentShell::PowerShell, 7897)
            .is_some_and(|value| value.starts_with("$env:")));
        assert!(proxy_environment(EnvironmentShell::Fish, 7897)
            .is_some_and(|value| value.starts_with("set -x")));
        assert!(proxy_environment(EnvironmentShell::Nushell, 7897)
            .is_some_and(|value| value.starts_with("$env.")));
    }

    #[test]
    fn environment_copy_refuses_an_unknown_runtime_port() {
        assert!(proxy_environment(EnvironmentShell::Bash, 0).is_none());
    }
}
