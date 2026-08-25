use super::{div, info_row, setting_card, yes_no, IntoElement, ParentElement, RuntimePage, Styled};

impl RuntimePage {
    pub(super) fn render_sniffer(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
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
}
