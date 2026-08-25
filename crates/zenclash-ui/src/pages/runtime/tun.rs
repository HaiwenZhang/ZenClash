use super::{
    empty_dash, info_row, json, setting_card, setting_switch, v_flex, yes_no, Context, IntoElement,
    ParentElement, RuntimePage, Styled,
};

impl RuntimePage {
    pub(super) fn render_tun(
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
                            );
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
}
