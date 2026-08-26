use super::{
    config_input_row, div, h_flex, json, setting_card, setting_switch, v_flex, Button,
    ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement, RuntimePage,
    Styled,
};

impl RuntimePage {
    pub(super) fn render_sniffer(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_sniffer_switches(theme, cx))
            .child(self.render_sniffer_filters(theme, cx))
            .into_any_element()
    }

    fn render_sniffer_switches(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let current = self.config().cloned().unwrap_or_default().sniffing;
        setting_card("域名嗅探运行状态", theme)
            .child(setting_switch(
                "嗅探",
                "识别连接中的真实域名",
                self.controlled_bool("/sniffer/enable", current.enable),
                "sniffer-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool("enable", *checked, "域名嗅探已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "强制 DNS 映射",
                "为嗅探流量强制使用 DNS 映射",
                self.controlled_bool("/sniffer/force-dns-mapping", current.force_dns_mapping),
                "sniffer-force-dns-mapping",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "force-dns-mapping",
                        *checked,
                        "强制 DNS 映射已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "解析纯 IP",
                "尝试从纯 IP 连接恢复域名",
                self.controlled_bool("/sniffer/parse-pure-ip", current.parse_pure_ip),
                "sniffer-parse-pure-ip",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "parse-pure-ip",
                        *checked,
                        "纯 IP 嗅探已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "覆盖目标地址",
                "用嗅探出的域名覆盖原目标地址",
                self.controlled_bool(
                    "/sniffer/override-destination",
                    current.override_destination,
                ),
                "sniffer-override-destination",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "override-destination",
                        *checked,
                        "目标地址覆盖已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(
                div()
                    .p_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("设置写入独立受控覆写层，不修改订阅源文件。"),
            )
    }

    fn patch_sniffer_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.apply_controlled_config(json!({"sniffer": {key: value}}), success, cx);
    }

    fn render_sniffer_filters(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.sniffer;
        setting_card("协议端口与过滤列表", theme)
            .child(config_input_row(
                "HTTP 端口",
                "逗号分隔，支持端口范围",
                Input::new(&inputs.http_ports),
                theme,
            ))
            .child(config_input_row(
                "TLS 端口",
                "逗号分隔，支持端口范围",
                Input::new(&inputs.tls_ports),
                theme,
            ))
            .child(config_input_row(
                "QUIC 端口",
                "逗号分隔，支持端口范围",
                Input::new(&inputs.quic_ports),
                theme,
            ))
            .child(config_input_row(
                "跳过域名",
                "每行一个域名",
                Input::new(&inputs.skip_domain),
                theme,
            ))
            .child(config_input_row(
                "强制域名",
                "每行一个域名",
                Input::new(&inputs.force_domain),
                theme,
            ))
            .child(config_input_row(
                "跳过目标地址",
                "每行一个 IP 或 CIDR",
                Input::new(&inputs.skip_dst_address),
                theme,
            ))
            .child(config_input_row(
                "跳过来源地址",
                "每行一个 IP 或 CIDR",
                Input::new(&inputs.skip_src_address),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-sniffer-advanced")
                        .icon(IconName::Check)
                        .label("保存嗅探高级配置")
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let patch = this.config_inputs.sniffer.patch(cx);
                            this.apply_controlled_config(patch, "嗅探高级配置已保存并热重载", cx);
                        })),
                ),
            )
    }
}
