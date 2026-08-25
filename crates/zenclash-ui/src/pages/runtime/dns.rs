use super::{
    empty_dash, info_row, message_banner, setting_card, v_flex, yes_no, IntoElement, ParentElement,
    RuntimePage, Styled,
};

impl RuntimePage {
    pub(super) fn render_dns(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
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
}
