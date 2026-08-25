use super::{
    format_port, h_flex, info_row, json, metric, setting_card, setting_switch, v_flex, Context,
    FluentBuilder, IntoElement, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Styled,
    VersionInfo,
};

impl RuntimePage {
    #[allow(
        clippy::too_many_lines,
        reason = "the core settings card is a single declarative GPUI element tree"
    )]
    pub(super) fn render_core(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (version, config) = match &self.data {
            RuntimeData::Core { version, config } => (version.clone(), config.clone()),
            _ => (VersionInfo::default(), RuntimeConfig::default()),
        };
        let process = self.process.as_ref().map(|process| process.snapshot());
        let process_status = process.as_ref().map_or_else(
            || "连接到外部内核".into(),
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
                            this.patch_config(json!({"ipv6": *checked}), "IPv6 设置已更新", cx);
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
                            this.patch_config(
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
                            this.patch_config(
                                json!({"unified-delay": *checked}),
                                "统一延迟设置已更新",
                                cx,
                            );
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
}
