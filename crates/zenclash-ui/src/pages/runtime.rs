use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    div, prelude::FluentBuilder, px, App, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    switch::Switch,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable,
};
use serde_json::{json, Value};
use zenclash_core::{
    format_speed, merge_profile_overrides, ConnectionsSnapshot, LogMonitor, MihomoClient,
    MihomoProcess, ProviderCatalog, RuleCatalog, RuntimeConfig, SubStoreClient, SubStoreItem,
    SubStoreSnapshot, SystemNetworkSnapshot, SystemProxyManager, SystemProxyStatus, TrafficMonitor,
    VersionInfo,
};

use crate::app::{HideTrafficIcon, SetDarkTheme, SetLightTheme, ShowTrafficIcon};

use super::Page;

#[derive(Clone, Debug)]
enum RuntimeData {
    Empty,
    Config(RuntimeConfig),
    Core {
        version: VersionInfo,
        config: RuntimeConfig,
    },
    Profile {
        config: RuntimeConfig,
        proxy_count: usize,
        group_count: usize,
        rule_count: usize,
    },
    Connections(ConnectionsSnapshot),
    Rules(RuleCatalog),
    Resources {
        proxy: ProviderCatalog,
        rules: ProviderCatalog,
    },
    SystemProxy {
        config: RuntimeConfig,
        status: SystemProxyStatus,
    },
    Network {
        config: RuntimeConfig,
        system: SystemNetworkSnapshot,
    },
    SubStore(SubStoreSnapshot),
}

pub struct RuntimePage {
    page: Page,
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    log_monitor: Arc<LogMonitor>,
    process: Option<Arc<MihomoProcess>>,
    profile_path: Option<PathBuf>,
    override_paths: Vec<PathBuf>,
    data: RuntimeData,
    traffic_samples: VecDeque<u64>,
    loading: bool,
    mutating: bool,
    error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui::FocusHandle,
}

impl RuntimePage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        page: Page,
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        traffic_monitor: Arc<TrafficMonitor>,
        log_monitor: Arc<LogMonitor>,
        process: Option<Arc<MihomoProcess>>,
        profile_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            page,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            process,
            profile_path,
            override_paths: Vec::new(),
            data: RuntimeData::Empty,
            traffic_samples: VecDeque::from(vec![0; 48]),
            loading: false,
            mutating: false,
            error: None,
            notice: None,
            focus_handle: cx.focus_handle(),
        };
        this.refresh(cx);
        this.start_live_updates(cx);
        this
    }

    pub fn switch_to(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.data = RuntimeData::Empty;
        self.error = None;
        self.notice = None;
        self.refresh(cx);
    }

    fn start_live_updates(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if this
                .update(cx, |this, cx| {
                    let traffic = this.traffic_monitor.snapshot();
                    if this.traffic_samples.len() >= 48 {
                        this.traffic_samples.pop_front();
                    }
                    this.traffic_samples
                        .push_back(traffic.upload.saturating_add(traffic.download));
                    if matches!(this.page, Page::Logs) {
                        cx.notify();
                    }
                    if matches!(this.page, Page::Connections | Page::Traffic)
                        && !this.loading
                        && !this.mutating
                    {
                        this.refresh(cx);
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let page = self.page;
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(async move { load_page(client, page).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("页面数据任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                if this.page == page {
                    this.loading = false;
                    match result {
                        Ok(data) => this.data = data,
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn patch_config(&mut self, body: Value, success: &'static str, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        self.mutating = true;
        self.error = None;
        self.notice = None;
        let page = self.page;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .patch_configs(&body)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, page).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("配置更新任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some(success.into());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reload_profile(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.profile_path.clone() else {
            self.error = Some("未配置当前配置文件路径".into());
            cx.notify();
            return;
        };
        if self.mutating {
            return;
        }
        self.mutating = true;
        self.error = None;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .reload_config(&path.to_string_lossy(), true)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Profiles).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("重载配置任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some("真实配置已由 Mihomo 热重载".into());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_profile(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 Mihomo YAML 配置".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.profile_path = Some(path);
                        this.reload_profile(cx);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.error = Some(format!("无法打开配置选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(format!("配置选择器异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn choose_overrides(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("按应用顺序选择 YAML 覆写".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    this.override_paths = paths;
                    this.notice = Some(format!(
                        "已选择 {} 份覆写；点击“合并并热重载”应用",
                        this.override_paths.len()
                    ));
                    this.error = None;
                    cx.notify();
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.error = Some(format!("无法打开覆写选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(format!("覆写选择器异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply_overrides(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some("未配置基础配置文件路径".into());
            cx.notify();
            return;
        };
        if self.override_paths.is_empty() || self.mutating {
            return;
        }
        self.mutating = true;
        self.error = None;
        self.notice = None;
        let overrides = self.override_paths.clone();
        let count = overrides.len();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let payload =
                tokio::task::spawn_blocking(move || merge_profile_overrides(profile, &overrides))
                    .await
                    .map_err(|error| format!("覆写合并任务异常结束：{error}"))?
                    .map_err(|error| error.to_string())?;
            client
                .reload_payload(payload, true)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Override).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("覆写热重载任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some(format!("{count} 份 YAML 覆写已合并并热重载"));
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_all_connections(&mut self, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        self.mutating = true;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_all_connections()
                .await
                .map_err(|error| error.to_string())?;
            client
                .connections_snapshot()
                .await
                .map(RuntimeData::Connections)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("关闭连接任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some("全部连接已关闭".into());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_connection(&mut self, id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_connection(&id)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("关闭连接任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.refresh(cx),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_provider(&mut self, name: String, is_rule: bool, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        self.mutating = true;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            if is_rule {
                client.update_rule_provider(&name).await
            } else {
                client.update_proxy_provider(&name).await
            }
            .map_err(|error| error.to_string())?;
            load_page(client, Page::Resources).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("更新外部资源任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some("外部资源已更新".into());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_system_proxy(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        let port = self
            .config()
            .map(|config| [config.mixed_port, config.port, config.socks_port])
            .and_then(|ports| ports.into_iter().find(|port| *port > 0))
            .unwrap_or_default();
        if enabled && port == 0 {
            self.error = Some("Mihomo 当前没有可用的 HTTP/Mixed 监听端口，无法启用系统代理".into());
            cx.notify();
            return;
        }

        self.mutating = true;
        self.error = None;
        self.notice = None;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
                manager
                    .set_enabled(enabled, "127.0.0.1", port)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            load_page(client, Page::SystemProxy).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("系统代理任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some(if enabled {
                            "系统 HTTP/HTTPS 代理已启用".into()
                        } else {
                            "系统 HTTP/HTTPS 代理已停用".into()
                        });
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_header(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.loading;
        h_flex()
            .h(px(49.))
            .px_5()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.page.label()),
            )
            .child(
                Button::new("refresh-runtime-page")
                    .icon(IconName::Redo2)
                    .label(if loading { "读取中" } else { "刷新" })
                    .small()
                    .loading(loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_status(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        v_flex()
            .gap_2()
            .when_some(self.error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(message_banner(notice, theme.success, theme))
            })
            .into_any_element()
    }

    fn render_body(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.page {
            Page::Mihomo => self.render_core(theme, cx),
            Page::Profiles => self.render_profile(theme, cx),
            Page::Connections => self.render_connections(theme, cx),
            Page::Rules => self.render_rules(theme),
            Page::Resources => self.render_resources(theme, cx),
            Page::Logs => self.render_logs(theme),
            Page::Tun => self.render_tun(theme, cx),
            Page::Sniffer => self.render_sniffer(theme),
            Page::Traffic => self.render_traffic(theme),
            Page::Network => self.render_network(theme),
            Page::Dns => self.render_dns(theme),
            Page::SystemProxy => self.render_system_proxy(theme, cx),
            Page::Override => self.render_override(theme, cx),
            Page::SubStore => self.render_substore(theme, cx),
            Page::Settings => self.render_settings(theme, cx),
            Page::Proxies => div().into_any_element(),
        }
    }

    fn config(&self) -> Option<&RuntimeConfig> {
        match &self.data {
            RuntimeData::Config(config)
            | RuntimeData::Core { config, .. }
            | RuntimeData::Profile { config, .. }
            | RuntimeData::SystemProxy { config, .. }
            | RuntimeData::Network { config, .. } => Some(config),
            _ => None,
        }
    }

    fn render_core(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (version, config) = match &self.data {
            RuntimeData::Core { version, config } => (version.clone(), config.clone()),
            _ => (VersionInfo::default(), RuntimeConfig::default()),
        };
        let process = self.process.as_ref().map(|process| process.snapshot());
        let process_status = process
            .as_ref()
            .map(|snapshot| {
                if snapshot.running {
                    format!("运行中 · PID {}", snapshot.pid.unwrap_or_default())
                } else {
                    "已停止".into()
                }
            })
            .unwrap_or_else(|| "连接到外部内核".into());

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric("内核版本", version.version, theme.primary, theme))
                    .child(metric("运行状态", process_status, theme.success, theme))
                    .child(metric(
                        "运行模式",
                        config.mode.clone(),
                        theme.warning,
                        theme,
                    )),
            )
            .child(
                setting_card("运行时开关", theme)
                    .child(setting_switch(
                        "IPv6",
                        "允许 Mihomo 解析和使用 IPv6",
                        config.ipv6,
                        "runtime-ipv6",
                        theme,
                        cx.listener(|this, checked, _, cx| {
                            this.patch_config(json!({"ipv6": *checked}), "IPv6 设置已更新", cx)
                        }),
                    ))
                    .child(setting_switch(
                        "允许局域网",
                        "允许其他设备访问监听端口",
                        config.allow_lan,
                        "runtime-allow-lan",
                        theme,
                        cx.listener(|this, checked, _, cx| {
                            this.patch_config(
                                json!({"allow-lan": *checked}),
                                "局域网访问设置已更新",
                                cx,
                            )
                        }),
                    ))
                    .child(setting_switch(
                        "TCP 并发",
                        "并行建立目标连接以降低握手等待",
                        config.tcp_concurrent,
                        "runtime-tcp-concurrent",
                        theme,
                        cx.listener(|this, checked, _, cx| {
                            this.patch_config(
                                json!({"tcp-concurrent": *checked}),
                                "TCP 并发设置已更新",
                                cx,
                            )
                        }),
                    ))
                    .child(setting_switch(
                        "统一延迟",
                        "使用统一的延迟计算方式",
                        config.unified_delay,
                        "runtime-unified-delay",
                        theme,
                        cx.listener(|this, checked, _, cx| {
                            this.patch_config(
                                json!({"unified-delay": *checked}),
                                "统一延迟设置已更新",
                                cx,
                            )
                        }),
                    )),
            )
            .child(
                setting_card("控制器与监听", theme)
                    .child(info_row(
                        "External Controller",
                        &self.client.endpoint().controller,
                        theme,
                    ))
                    .child(info_row("HTTP", &format_port(config.port), theme))
                    .child(info_row("SOCKS", &format_port(config.socks_port), theme))
                    .child(info_row("Mixed", &format_port(config.mixed_port), theme))
                    .child(info_row("日志等级", &config.log_level, theme)),
            )
            .when_some(process, |this, snapshot| {
                this.child(
                    setting_card("内核进程", theme)
                        .child(info_row(
                            "二进制",
                            &snapshot.binary.display().to_string(),
                            theme,
                        ))
                        .child(info_row(
                            "配置文件",
                            &snapshot.config_file.display().to_string(),
                            theme,
                        ))
                        .child(info_row(
                            "工作目录",
                            &snapshot.home_dir.display().to_string(),
                            theme,
                        )),
                )
            })
            .into_any_element()
    }

    fn render_profile(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, proxy_count, group_count, rule_count) = match &self.data {
            RuntimeData::Profile {
                config,
                proxy_count,
                group_count,
                rule_count,
            } => (config.clone(), *proxy_count, *group_count, *rule_count),
            _ => (RuntimeConfig::default(), 0, 0, 0),
        };
        let path = self
            .profile_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未指定".into());
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "代理对象",
                        proxy_count.to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "策略组",
                        group_count.to_string(),
                        theme.success,
                        theme,
                    ))
                    .child(metric("规则", rule_count.to_string(), theme.warning, theme)),
            )
            .child(
                setting_card("当前真实配置", theme)
                    .child(info_row("配置路径", &path, theme))
                    .child(info_row("运行模式", &config.mode, theme))
                    .child(info_row("日志等级", &config.log_level, theme))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .p_3()
                            .child(
                                Button::new("choose-profile")
                                    .icon(IconName::FolderOpen)
                                    .label("选择本地配置")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.choose_profile(cx)),
                                    ),
                            )
                            .child(
                                Button::new("reload-profile")
                                    .icon(IconName::Redo2)
                                    .label("热重载配置")
                                    .primary()
                                    .loading(self.mutating)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.reload_profile(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_connections(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let data = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let total = data.connections.len();
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric("活动连接", total.to_string(), theme.primary, theme))
                    .child(metric(
                        "累计上传",
                        format_bytes(data.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "累计下载",
                        format_bytes(data.download_total),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "内存",
                        format_bytes(data.memory),
                        theme.warning,
                        theme,
                    )),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("连接数据每 500ms 从真实控制器刷新"),
                    )
                    .child(
                        Button::new("close-all-connections")
                            .icon(IconName::CircleX)
                            .label("关闭全部")
                            .danger()
                            .small()
                            .disabled(total == 0 || self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx))),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(total == 0, |this| {
                        this.child(empty_state("当前没有活动连接", theme))
                    })
                    .children(
                        data.connections
                            .iter()
                            .enumerate()
                            .map(|(index, connection)| {
                                let id = connection.id.clone();
                                let host = if connection.metadata.host.is_empty() {
                                    connection.metadata.destination_ip.clone()
                                } else {
                                    connection.metadata.host.clone()
                                };
                                let chain = connection.chains.join(" → ");
                                h_flex()
                                    .id(("connection-row", index))
                                    .min_h(px(58.))
                                    .px_4()
                                    .gap_3()
                                    .items_center()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(Icon::new(IconName::ExternalLink).size_4())
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().text_sm().child(format!(
                                                "{}:{}",
                                                host, connection.metadata.destination_port
                                            )))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(format!(
                                                        "{} · {} · {}",
                                                        connection.metadata.network,
                                                        connection.rule,
                                                        chain
                                                    )),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .items_end()
                                            .text_xs()
                                            .child(format!("↑ {}", format_bytes(connection.upload)))
                                            .child(format!(
                                                "↓ {}",
                                                format_bytes(connection.download)
                                            )),
                                    )
                                    .child(
                                        Button::new(("close-connection", index))
                                            .icon(IconName::CircleX)
                                            .ghost()
                                            .small()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.close_connection(id.clone(), cx)
                                            })),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_rules(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let rules = match &self.data {
            RuntimeData::Rules(data) => data.rules.clone(),
            _ => Vec::new(),
        };
        let visible = rules.len().min(800);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(metric(
                        "运行时规则",
                        rules.len().to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("显示前 {visible} 条")),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .children(
                        rules
                            .into_iter()
                            .take(visible)
                            .enumerate()
                            .map(|(index, rule)| {
                                h_flex()
                                    .id(("rule-row", index))
                                    .min_h(px(42.))
                                    .px_4()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .w(px(116.))
                                            .text_xs()
                                            .text_color(theme.primary)
                                            .child(rule.kind),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(rule.payload),
                                    )
                                    .child(
                                        div()
                                            .w(px(190.))
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(rule.proxy),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_resources(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (proxy, rules) = match &self.data {
            RuntimeData::Resources { proxy, rules } => (proxy.clone(), rules.clone()),
            _ => (ProviderCatalog::default(), ProviderCatalog::default()),
        };
        v_flex()
            .gap_4()
            .child(provider_section(
                "代理提供者",
                proxy,
                false,
                self.mutating,
                theme,
                cx,
            ))
            .child(provider_section(
                "规则提供者",
                rules,
                true,
                self.mutating,
                theme,
                cx,
            ))
            .into_any_element()
    }

    fn render_logs(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let entries = self.log_monitor.entries();
        let connected = self.log_monitor.connected();
        let visible = entries.len().min(600);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(metric(
                        "日志条目",
                        entries.len().to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(if connected {
                                theme.success
                            } else {
                                theme.danger
                            })
                            .child(div().size_2().rounded_full().bg(if connected {
                                theme.success
                            } else {
                                theme.danger
                            }))
                            .child(if connected {
                                "实时流已连接"
                            } else {
                                "正在重连"
                            }),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(entries.is_empty(), |this| {
                        this.child(empty_state("等待 Mihomo 日志事件…", theme))
                    })
                    .children(entries.into_iter().rev().take(visible).enumerate().map(
                        |(index, entry)| {
                            let color = match entry.level.as_str() {
                                "error" => theme.danger,
                                "warning" | "warn" => theme.warning,
                                "debug" => theme.muted_foreground,
                                _ => theme.success,
                            };
                            h_flex()
                                .id(("log-row", index))
                                .items_start()
                                .gap_3()
                                .px_4()
                                .py_2()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .w(px(62.))
                                        .text_xs()
                                        .text_color(color)
                                        .child(entry.level.to_uppercase()),
                                )
                                .child(div().flex_1().text_xs().child(entry.payload))
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_tun(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let config = self.config().cloned().unwrap_or_default();
        let tun = config.tun;
        v_flex()
            .gap_4()
            .child(
                setting_card("虚拟网卡", theme)
                    .child(setting_switch(
                        "启用 TUN",
                        "通过 Mihomo TUN 接管系统网络流量",
                        tun.enable,
                        "tun-enable",
                        theme,
                        cx.listener(|this, checked, _, cx| {
                            this.patch_config(
                                json!({"tun": {"enable": *checked}}),
                                "TUN 状态已更新",
                                cx,
                            )
                        }),
                    ))
                    .child(info_row("网络栈", &tun.stack, theme))
                    .child(info_row("设备", &empty_dash(&tun.device), theme))
                    .child(info_row("DNS 劫持", &tun.dns_hijack.join(", "), theme))
                    .child(info_row("自动路由", yes_no(tun.auto_route), theme))
                    .child(info_row(
                        "自动检测接口",
                        yes_no(tun.auto_detect_interface),
                        theme,
                    ))
                    .child(info_row("严格路由", yes_no(tun.strict_route), theme)),
            )
            .into_any_element()
    }

    fn render_sniffer(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let sniffer = self.config().cloned().unwrap_or_default().sniffing;
        setting_card("域名嗅探运行状态", theme)
            .child(info_row("嗅探", yes_no(sniffer.enable), theme))
            .child(info_row(
                "强制 DNS 映射",
                yes_no(sniffer.force_dns_mapping),
                theme,
            ))
            .child(info_row("解析纯 IP", yes_no(sniffer.parse_pure_ip), theme))
            .child(info_row(
                "覆盖目标地址",
                yes_no(sniffer.override_destination),
                theme,
            ))
            .child(
                div()
                    .p_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("嗅探协议与域名列表来自当前真实配置文件；修改后可在“订阅管理”热重载。"),
            )
            .into_any_element()
    }

    fn render_traffic(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let realtime = self.traffic_monitor.snapshot();
        let connections = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let maximum = self
            .traffic_samples
            .iter()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "实时上传",
                        format_speed(realtime.upload),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "实时下载",
                        format_speed(realtime.download),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "累计上传",
                        format_bytes(connections.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "累计下载",
                        format_bytes(connections.download_total),
                        theme.primary,
                        theme,
                    )),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(
                        div()
                            .px_4()
                            .pt_4()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("最近 24 秒实时吞吐"),
                    )
                    .child(
                        h_flex().h(px(180.)).items_end().gap_1().p_4().children(
                            self.traffic_samples
                                .iter()
                                .enumerate()
                                .map(|(index, value)| {
                                    div()
                                        .id(("traffic-bar", index))
                                        .flex_1()
                                        .h(px(4. + 144. * (*value as f32 / maximum as f32)))
                                        .rounded_sm()
                                        .bg(theme.primary.opacity(0.7))
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_network(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let (config, system) = match &self.data {
            RuntimeData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "控制器",
                        self.client.endpoint().controller.clone(),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "本地 IPv4",
                        empty_dash(&system.local_ipv4),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "出口接口",
                        empty_dash(&system.interface),
                        theme.warning,
                        theme,
                    )),
            )
            .child(
                setting_card("默认网络路径", theme)
                    .child(info_row("接口", &system.interface, theme))
                    .child(info_row("网关", &system.gateway, theme))
                    .child(info_row("本地地址", &system.local_ipv4, theme))
                    .child(info_row("DNS", &system.dns_servers.join(", "), theme))
                    .when_some(system.error, |this, error| {
                        this.child(message_banner(error, theme.warning, theme))
                    }),
            )
            .child(
                setting_card("网络能力", theme)
                    .child(info_row("IPv6", yes_no(config.ipv6), theme))
                    .child(info_row("允许局域网", yes_no(config.allow_lan), theme))
                    .child(info_row("TCP 并发", yes_no(config.tcp_concurrent), theme))
                    .child(info_row("统一延迟", yes_no(config.unified_delay), theme)),
            )
            .into_any_element()
    }

    fn render_dns(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let config = self.config().cloned().unwrap_or_default();
        let dns_hijack = config.tun.dns_hijack.join(", ");
        v_flex()
            .gap_4()
            .child(
                setting_card("DNS 运行状态", theme)
                    .child(info_row("TUN DNS 劫持", &empty_dash(&dns_hijack), theme))
                    .child(info_row("IPv6 解析", yes_no(config.ipv6), theme))
                    .child(info_row(
                        "嗅探 DNS 映射",
                        yes_no(config.sniffing.force_dns_mapping),
                        theme,
                    )),
            )
            .child(message_banner(
                "完整 Nameserver、Fallback 与 Fake-IP 列表由当前 YAML 提供，并随配置热重载。"
                    .into(),
                theme.primary,
                theme,
            ))
            .into_any_element()
    }

    fn render_system_proxy(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, status) = match &self.data {
            RuntimeData::SystemProxy { config, status } => (config.clone(), status.clone()),
            _ => (RuntimeConfig::default(), SystemProxyStatus::default()),
        };
        let active = status.enabled && status.secure_enabled;
        let port = [config.mixed_port, config.port, config.socks_port]
            .into_iter()
            .find(|port| *port > 0)
            .unwrap_or_default();
        setting_card("系统代理", theme)
            .child(setting_switch(
                "启用系统代理",
                "同步控制 macOS HTTP 与 HTTPS 代理",
                active,
                "system-proxy-enable",
                theme,
                cx.listener(|this, checked, _, cx| this.toggle_system_proxy(*checked, cx)),
            ))
            .child(info_row("网络服务", &status.service, theme))
            .child(info_row(
                "当前 HTTP",
                &format_proxy(&status.server, status.port, status.enabled),
                theme,
            ))
            .child(info_row(
                "当前 HTTPS",
                &format_proxy(
                    &status.secure_server,
                    status.secure_port,
                    status.secure_enabled,
                ),
                theme,
            ))
            .child(info_row("Mihomo 代理端口", &format_port(port), theme))
            .into_any_element()
    }

    fn render_override(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = self
            .profile_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未指定".into());
        let count = self.override_paths.len();
        v_flex()
            .gap_4()
            .child(
                setting_card("配置覆写链", theme)
                    .child(info_row("基础配置", &path, theme))
                    .child(info_row("YAML 覆写", &format!("{count} 份"), theme))
                    .children(self.override_paths.iter().enumerate().map(|(index, path)| {
                        info_row(
                            "应用顺序",
                            &format!("{}. {}", index + 1, path.display()),
                            theme,
                        )
                    }))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .p_3()
                            .child(
                                Button::new("choose-overrides")
                                    .icon(IconName::FolderOpen)
                                    .label("选择 YAML 覆写")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.choose_overrides(cx)),
                                    ),
                            )
                            .child(
                                Button::new("clear-overrides")
                                    .label("清空")
                                    .disabled(count == 0 || self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.override_paths.clear();
                                        this.notice =
                                            Some("覆写选择已清空；运行中配置未改变".into());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("apply-overrides")
                                    .icon(IconName::Redo2)
                                    .label("合并并热重载")
                                    .primary()
                                    .loading(self.mutating)
                                    .disabled(count == 0)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.apply_overrides(cx)),
                                    ),
                            ),
                    ),
            )
            .child(message_banner(
                "基础文件保持不变；映射递归合并，后选覆写优先，数组与标量整体替换。".into(),
                theme.primary,
                theme,
            ))
            .into_any_element()
    }

    fn render_substore(
        &self,
        theme: &gpui_component::Theme,
        _cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let snapshot = match &self.data {
            RuntimeData::SubStore(snapshot) => snapshot.clone(),
            _ => SubStoreSnapshot::default(),
        };
        let frontend_url = snapshot.frontend_url.clone();
        v_flex()
            .gap_4()
            .child(
                setting_card("Sub-Store 服务", theme)
                    .child(info_row(
                        "后端服务",
                        if snapshot.connected {
                            "已连接"
                        } else {
                            "等待连接"
                        },
                        theme,
                    ))
                    .child(info_row("后端地址", &snapshot.backend_url, theme))
                    .child(info_row("前端地址", &snapshot.frontend_url, theme))
                    .child(
                        h_flex().justify_end().p_3().child(
                            Button::new("open-substore")
                                .icon(IconName::ExternalLink)
                                .label("在浏览器中打开")
                                .disabled(frontend_url.is_empty())
                                .on_click(move |_, _, cx| cx.open_url(&frontend_url)),
                        ),
                    ),
            )
            .when_some(snapshot.error, |this, error| {
                this.child(message_banner(
                    format!(
                        "未连接 Sub-Store：{error}。可通过 ZENCLASH_SUBSTORE_URL 和 ZENCLASH_SUBSTORE_FRONTEND_URL 接入现有服务。"
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(substore_items(
                "订阅",
                snapshot.subscriptions,
                theme.primary,
                theme,
            ))
            .child(substore_items(
                "集合",
                snapshot.collections,
                theme.success,
                theme,
            ))
            .into_any_element()
    }

    fn render_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let config = self.config().cloned().unwrap_or_default();
        setting_card("应用与控制器", theme)
            .child(info_row("界面技术", "Rust · GPUI · gpui-component", theme))
            .child(info_row(
                "控制器",
                &self.client.endpoint().controller,
                theme,
            ))
            .child(setting_switch(
                "IPv6",
                "同步修改 Mihomo 运行时设置",
                config.ipv6,
                "settings-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_config(json!({"ipv6": *checked}), "IPv6 设置已更新", cx)
                }),
            ))
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex().gap_1().child(div().text_sm().child("主题")).child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("切换 GPUI 原生明暗外观"),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("theme-light")
                                    .icon(IconName::Sun)
                                    .label("浅色")
                                    .small()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetLightTheme), cx)
                                    }),
                            )
                            .child(
                                Button::new("theme-dark")
                                    .icon(IconName::Moon)
                                    .label("深色")
                                    .small()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SetDarkTheme), cx)
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child("状态栏流量图标"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("显示实时上传、下载速率和动态箭头图标"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("tray-show").label("显示").small().on_click(
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(ShowTrafficIcon), cx)
                                },
                            ))
                            .child(Button::new("tray-hide").label("隐藏").small().on_click(
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(HideTrafficIcon), cx)
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for RuntimePage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RuntimePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_4()
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(self.page.label()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(self.page.subtitle()),
                            ),
                    )
                    .child(self.render_status(&theme))
                    .child(self.render_body(&theme, cx)),
            )
    }
}

async fn load_page(client: MihomoClient, page: Page) -> Result<RuntimeData, String> {
    match page {
        Page::Mihomo => {
            let (version, config) = tokio::try_join!(client.version(), client.runtime_config())
                .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Core { version, config })
        }
        Page::Profiles => {
            let (config, proxies, rules) = tokio::try_join!(
                client.runtime_config(),
                client.proxy_catalog(),
                client.rule_catalog()
            )
            .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Profile {
                config,
                proxy_count: proxies.proxy_count,
                group_count: proxies.groups.len(),
                rule_count: rules.rules.len(),
            })
        }
        Page::Connections | Page::Traffic => client
            .connections_snapshot()
            .await
            .map(RuntimeData::Connections)
            .map_err(|error| error.to_string()),
        Page::Rules => client
            .rule_catalog()
            .await
            .map(RuntimeData::Rules)
            .map_err(|error| error.to_string()),
        Page::Resources => {
            let (proxy, rules) = tokio::try_join!(
                client.proxy_provider_catalog(),
                client.rule_provider_catalog()
            )
            .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Resources { proxy, rules })
        }
        Page::SystemProxy => {
            let config = client
                .runtime_config()
                .await
                .map_err(|error| error.to_string())?;
            let status = tokio::task::spawn_blocking(|| {
                let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
                manager.status().map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            Ok(RuntimeData::SystemProxy { config, status })
        }
        Page::Network => {
            let config = client
                .runtime_config()
                .await
                .map_err(|error| error.to_string())?;
            let system = tokio::task::spawn_blocking(SystemNetworkSnapshot::detect)
                .await
                .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Network { config, system })
        }
        Page::SubStore => {
            let client = SubStoreClient::from_env().map_err(|error| error.to_string())?;
            Ok(RuntimeData::SubStore(client.snapshot().await))
        }
        Page::Logs => Ok(RuntimeData::Empty),
        _ => client
            .runtime_config()
            .await
            .map(RuntimeData::Config)
            .map_err(|error| error.to_string()),
    }
}

fn provider_section(
    title: &'static str,
    catalog: ProviderCatalog,
    is_rule: bool,
    mutating: bool,
    theme: &gpui_component::Theme,
    cx: &mut Context<RuntimePage>,
) -> gpui::AnyElement {
    let count = catalog.providers.len();
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{count} 项")),
                ),
        )
        .child(
            v_flex()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .when(count == 0, |this| {
                    this.child(empty_state("没有提供者", theme))
                })
                .children(catalog.providers.into_iter().enumerate().map(
                    |(index, (key, provider))| {
                        let name = if provider.name.is_empty() {
                            key
                        } else {
                            provider.name
                        };
                        let name_for_click = name.clone();
                        let item_count = if is_rule {
                            provider.rule_count
                        } else {
                            provider.proxies.len()
                        };
                        h_flex()
                            .id((
                                if is_rule {
                                    "rule-provider"
                                } else {
                                    "proxy-provider"
                                },
                                index,
                            ))
                            .min_h(px(58.))
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Icon::new(IconName::Inbox).size_4())
                            .child(
                                v_flex().flex_1().child(div().text_sm().child(name)).child(
                                    div().text_xs().text_color(theme.muted_foreground).child(
                                        format!(
                                            "{} · {} · {} 项",
                                            provider.vehicle_type,
                                            empty_dash(&provider.updated_at),
                                            item_count
                                        ),
                                    ),
                                ),
                            )
                            .child(
                                Button::new(("update-provider", index))
                                    .icon(IconName::Redo2)
                                    .label("更新")
                                    .small()
                                    .disabled(mutating)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update_provider(name_for_click.clone(), is_rule, cx)
                                    })),
                            )
                    },
                )),
        )
        .into_any_element()
}

fn substore_items(
    title: &'static str,
    items: Vec<SubStoreItem>,
    accent: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let count = items.len();
    let id_prefix = if title == "订阅" {
        "substore-subscription"
    } else {
        "substore-collection"
    };
    setting_card(title, theme)
        .when(count == 0, |this| {
            this.child(empty_state("没有可显示的项目", theme))
        })
        .children(items.into_iter().enumerate().map(|(index, item)| {
            let label = if item.display_name.is_empty() {
                item.name
            } else {
                item.display_name
            };
            h_flex()
                .id((id_prefix, index))
                .min_h(px(48.))
                .px_4()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .child(div().size_2().rounded_full().bg(accent))
                .child(div().flex_1().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(item.tag.join(" · ")),
                )
        }))
        .into_any_element()
}

fn setting_card(title: &'static str, theme: &gpui_component::Theme) -> gpui::Div {
    v_flex()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            div()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
}

fn info_row(label: &'static str, value: &str, theme: &gpui_component::Theme) -> gpui::AnyElement {
    h_flex()
        .min_h(px(46.))
        .px_4()
        .gap_4()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(div().text_sm().child(label))
        .child(
            div()
                .max_w(px(620.))
                .text_right()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(empty_dash(value)),
        )
        .into_any_element()
}

fn setting_switch<F>(
    label: &'static str,
    description: &'static str,
    checked: bool,
    id: &'static str,
    theme: &gpui_component::Theme,
    listener: F,
) -> gpui::AnyElement
where
    F: Fn(&bool, &mut Window, &mut App) + 'static,
{
    h_flex()
        .min_h(px(58.))
        .px_4()
        .gap_4()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex().gap_1().child(div().text_sm().child(label)).child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(description),
            ),
        )
        .child(Switch::new(id).checked(checked).on_click(listener))
        .into_any_element()
}

fn metric(
    label: &'static str,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    v_flex()
        .min_w(px(190.))
        .flex_1()
        .gap_1()
        .p_4()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(div().text_xs().text_color(color).child(label))
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .child(value),
        )
        .into_any_element()
}

fn message_banner(
    message: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .gap_2()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(color.opacity(0.55))
        .bg(color.opacity(0.1))
        .text_sm()
        .text_color(color)
        .child(Icon::new(IconName::Info).size_4())
        .child(message)
        .into_any_element()
}

fn empty_state(message: &'static str, theme: &gpui_component::Theme) -> gpui::AnyElement {
    div()
        .p_5()
        .text_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element()
}

fn format_port(port: u16) -> String {
    if port == 0 {
        "未监听".into()
    } else {
        format!("127.0.0.1:{port}")
    }
}

fn format_proxy(server: &str, port: u16, enabled: bool) -> String {
    if !enabled {
        "已停用".into()
    } else if server.trim().is_empty() || port == 0 {
        "配置异常".into()
    } else {
        format!("{server}:{port}")
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KiB", bytes as f64 / KIB),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", bytes as f64 / MIB),
        _ => format!("{:.1} GiB", bytes as f64 / GIB),
    }
}

fn empty_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "已启用"
    } else {
        "已停用"
    }
}
