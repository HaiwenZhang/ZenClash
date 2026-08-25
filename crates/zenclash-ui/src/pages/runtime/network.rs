use super::{
    empty_dash, h_flex, info_row, message_banner, metric, setting_card, v_flex, yes_no,
    FluentBuilder, IntoElement, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Styled,
    SystemNetworkSnapshot,
};

impl RuntimePage {
    pub(super) fn render_network(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
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
}
