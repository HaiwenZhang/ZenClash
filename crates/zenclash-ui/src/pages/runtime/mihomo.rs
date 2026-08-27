use super::{
    config_input_row, format_port, h_flex, info_row, json, load_page, message_banner, metric,
    setting_card, setting_switch, v_flex, Button, ButtonVariants, Context, Disableable, Duration,
    FluentBuilder, IconName, Input, IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData,
    RuntimePage, Sizable, Styled, VersionInfo,
};

mod maintenance;

pub(super) use maintenance::CoreReleaseState;

impl RuntimePage {
    fn restart_managed_core(&mut self, cx: &mut Context<Self>) {
        let Some(process) = self.process.clone() else {
            self.error = Some("当前连接的是外部内核，ZenClash 无法重启该进程".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Mihomo) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let restarting = process.clone();
            tokio::task::spawn_blocking(move || restarting.restart())
                .await
                .map_err(|error| format!("内核重启任务异常结束：{error}"))?
                .map_err(|error| error.to_string())?;
            process
                .wait_until_ready(Duration::from_secs(20))
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Mihomo).await
        });
        Self::finish_core_maintenance(
            task,
            token,
            format!(
                "{} 内核已重启并通过 /version 验证",
                self.core_kind.display_name()
            ),
            cx,
        );
    }

    fn finish_core_maintenance(
        task: tokio::task::JoinHandle<Result<RuntimeData, String>>,
        token: super::PageTaskToken,
        success: String,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("内核维护任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(success);
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the core settings card is a single declarative GPUI element tree"
    )]
    pub(super) fn render_core(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (version, config, has_runtime_data) = match &self.data {
            RuntimeData::Core { version, config } => (version.clone(), config.clone(), true),
            _ => (VersionInfo::default(), RuntimeConfig::default(), false),
        };
        let process = self.process.as_ref().map(|process| process.snapshot());
        let managed_process = process.is_some();
        let process_running = process
            .as_ref()
            .map_or(has_runtime_data, |snapshot| snapshot.running);
        let process_status = process.as_ref().map_or_else(
            || {
                if has_runtime_data {
                    "连接到外部内核".into()
                } else {
                    "外部内核不可达".into()
                }
            },
            |snapshot| {
                if snapshot.running {
                    format!("运行中 · PID {}", snapshot.pid.unwrap_or_default())
                } else {
                    "已停止".into()
                }
            },
        );

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        "内核版本",
                        if has_runtime_data && !version.version.is_empty() {
                            version.version.clone()
                        } else {
                            "无法读取".into()
                        },
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "运行状态",
                        process_status,
                        if process_running {
                            theme.success
                        } else {
                            theme.danger
                        },
                        theme,
                    ))
                    .child(metric(
                        "运行模式",
                        if has_runtime_data && !config.mode.is_empty() {
                            config.mode.clone()
                        } else {
                            "无法读取".into()
                        },
                        theme.warning,
                        theme,
                    )),
            )
            .when(!has_runtime_data, |this| {
                this.child(message_banner(
                    "没有使用默认值冒充运行状态。请重启托管内核，或确认外部控制器地址和密钥后刷新。".into(),
                    theme.danger,
                    theme,
                ))
            })
            .when(has_runtime_data, |this| {
                this.child(
                    setting_card("运行时开关", theme)
                        .child(setting_switch(
                            "IPv6",
                            format!("允许 {} 解析和使用 IPv6", self.core_kind.display_name()),
                            config.ipv6,
                            "runtime-ipv6",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"ipv6": *checked}),
                                    "IPv6 设置已保存并热重载",
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            "允许局域网",
                            "允许其他设备访问监听端口",
                            config.allow_lan,
                            "runtime-allow-lan",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"allow-lan": *checked}),
                                    "局域网访问设置已更新",
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            "TCP 并发",
                            "并行建立目标连接以降低握手等待",
                            config.tcp_concurrent,
                            "runtime-tcp-concurrent",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"tcp-concurrent": *checked}),
                                    "TCP 并发设置已更新",
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            "统一延迟",
                            "使用统一的延迟计算方式",
                            config.unified_delay,
                            "runtime-unified-delay",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"unified-delay": *checked}),
                                    "统一延迟设置已更新",
                                    cx,
                                );
                            }),
                        )),
                )
            })
            .when(has_runtime_data, |this| {
                this.child(
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
            })
            .when(has_runtime_data, |this| {
                this.child(self.render_core_inputs(theme, cx))
            })
            .child(
                setting_card("内核维护", theme)
                    .child(info_row(
                        "升级通道",
                        if self.core_kind.capabilities().core_upgrade {
                            "官方 Release · SHA-256 校验 · 候选 -t 预检 · 失败回滚"
                        } else {
                            "当前内核不支持应用内升级"
                        },
                        theme,
                    ))
                    .child(info_row(
                        "重启能力",
                        if managed_process {
                            "由 ZenClash 管理并验证就绪"
                        } else {
                            "外部进程需由其服务管理器重启"
                        },
                        theme,
                    ))
                    .child(
                        h_flex().justify_end().gap_2().p_4().child(
                            Button::new("restart-mihomo-core")
                                .icon(IconName::Redo2)
                                .label("重启内核")
                                .small()
                                .outline()
                                .loading(self.mutating)
                                .disabled(self.mutating || !managed_process)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.restart_managed_core(cx);
                                })),
                        ),
                    ),
            )
            .child(self.render_versioned_core_updates(&version.version, managed_process, theme, cx))
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

    fn render_core_inputs(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.core;
        setting_card("监听端口与出口", theme)
            .child(config_input_row(
                "HTTP 端口",
                "0 表示停用独立 HTTP 监听",
                Input::new(&inputs.port),
                theme,
            ))
            .child(config_input_row(
                "SOCKS 端口",
                "0 表示停用独立 SOCKS 监听",
                Input::new(&inputs.socks_port),
                theme,
            ))
            .child(config_input_row(
                "Mixed 端口",
                "同时接受 HTTP 与 SOCKS；系统代理优先使用此端口",
                Input::new(&inputs.mixed_port),
                theme,
            ))
            .child(config_input_row(
                "Redir 端口",
                "Linux 透明代理 REDIRECT 监听端口",
                Input::new(&inputs.redir_port),
                theme,
            ))
            .child(config_input_row(
                "TPROXY 端口",
                "Linux TPROXY 透明代理监听端口",
                Input::new(&inputs.tproxy_port),
                theme,
            ))
            .child(config_input_row(
                "监听地址",
                "使用 127.0.0.1 仅允许本机访问；* 接受配置允许的地址",
                Input::new(&inputs.bind_address),
                theme,
            ))
            .child(config_input_row(
                "出口接口",
                "留空由当前内核自动选择；填写真实系统接口名称可固定出口",
                Input::new(&inputs.interface_name),
                theme,
            ))
            .child(config_input_row(
                "日志等级",
                "silent / error / warning / info / debug",
                Input::new(&inputs.log_level),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-core-listeners")
                        .icon(IconName::Check)
                        .label(format!("保存并由 {} 验证", self.core_kind.display_name()))
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.core.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    "监听端口、出口接口和日志等级已保存",
                                    cx,
                                ),
                                Err(error) => {
                                    this.error = Some(error);
                                    cx.notify();
                                }
                            }
                        })),
                ),
            )
    }
}
