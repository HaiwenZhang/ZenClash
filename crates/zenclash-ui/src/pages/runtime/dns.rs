use super::{
    config_input_row, empty_dash, h_flex, info_row, json, setting_card, setting_switch, v_flex,
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement,
    RuntimePage, Styled,
};

impl RuntimePage {
    pub(super) fn render_dns(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_dns_switches(theme, cx))
            .child(self.render_dns_resolvers(theme))
            .child(self.render_dns_policy(theme, cx))
            .into_any_element()
    }

    fn render_dns_switches(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let config = self.config().cloned().unwrap_or_default();
        setting_card("DNS 运行状态", theme)
            .child(setting_switch(
                "启用 DNS",
                "使用 Mihomo 内置 DNS 解析器",
                self.controlled_bool("/dns/enable", true),
                "dns-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool("enable", *checked, "DNS 状态已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "Fallback GeoIP",
                "根据 GeoIP 国家代码决定是否使用 Fallback",
                self.controlled_bool("/dns/fallback-filter/geoip", true),
                "dns-fallback-geoip",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"dns": {"fallback-filter": {"geoip": *checked}}}),
                        "Fallback GeoIP 已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "IPv6 解析",
                "允许 DNS 返回 AAAA 记录",
                self.controlled_bool("/dns/ipv6", false),
                "dns-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool("ipv6", *checked, "DNS IPv6 已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "使用 Hosts",
                "应用配置文件中的 hosts 映射",
                self.controlled_bool("/dns/use-hosts", true),
                "dns-use-hosts",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool("use-hosts", *checked, "DNS Hosts 设置已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "使用系统 Hosts",
                "读取操作系统 hosts 文件",
                self.controlled_bool("/dns/use-system-hosts", true),
                "dns-use-system-hosts",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "use-system-hosts",
                        *checked,
                        "系统 Hosts 设置已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "遵循规则",
                "DNS 查询遵循当前代理规则",
                self.controlled_bool("/dns/respect-rules", false),
                "dns-respect-rules",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "respect-rules",
                        *checked,
                        "DNS 规则策略已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(info_row(
                "TUN DNS 劫持",
                &empty_dash(&config.tun.dns_hijack.join(", ")),
                theme,
            ))
            .child(info_row(
                "嗅探 DNS 映射",
                if config.sniffing.force_dns_mapping {
                    "是"
                } else {
                    "否"
                },
                theme,
            ))
    }

    fn patch_dns_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.apply_controlled_config(json!({"dns": {key: value}}), success, cx);
    }

    fn render_dns_resolvers(&self, theme: &gpui_component::Theme) -> gpui::Div {
        let inputs = &self.config_inputs.dns;
        setting_card("解析器与 Fake-IP", theme)
            .child(config_input_row(
                "增强模式",
                "fake-ip / redir-host / normal",
                Input::new(&inputs.enhanced_mode),
                theme,
            ))
            .child(config_input_row(
                "Fake-IP 地址池",
                "Mihomo Fake-IP CIDR",
                Input::new(&inputs.fake_ip_range),
                theme,
            ))
            .child(config_input_row(
                "过滤模式",
                "blacklist / whitelist / rule",
                Input::new(&inputs.fake_ip_filter_mode),
                theme,
            ))
            .child(config_input_row(
                "Fake-IP 过滤",
                "每行一个域名、通配符或规则",
                Input::new(&inputs.fake_ip_filter),
                theme,
            ))
            .child(config_input_row(
                "默认解析器",
                "用于解析 DNS 服务器自身域名",
                Input::new(&inputs.default_nameserver),
                theme,
            ))
            .child(config_input_row(
                "Nameserver",
                "主要 DNS 解析器",
                Input::new(&inputs.nameserver),
                theme,
            ))
            .child(config_input_row(
                "代理解析器",
                "代理节点域名专用解析器",
                Input::new(&inputs.proxy_server_nameserver),
                theme,
            ))
            .child(config_input_row(
                "直连解析器",
                "直连请求专用解析器",
                Input::new(&inputs.direct_nameserver),
                theme,
            ))
            .child(config_input_row(
                "Fallback",
                "备用 DNS 解析器",
                Input::new(&inputs.fallback),
                theme,
            ))
    }

    fn render_dns_policy(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.dns;
        setting_card("Fallback、Policy 与 Hosts", theme)
            .child(config_input_row(
                "GeoIP 国家代码",
                "例如 CN",
                Input::new(&inputs.fallback_geoip_code),
                theme,
            ))
            .child(config_input_row(
                "Fallback IP CIDR",
                "每行一个 CIDR",
                Input::new(&inputs.fallback_ipcidr),
                theme,
            ))
            .child(config_input_row(
                "Fallback 域名",
                "每行一个域名规则",
                Input::new(&inputs.fallback_domain),
                theme,
            ))
            .child(config_input_row(
                "Nameserver Policy",
                "YAML 映射；值可为单个 DNS 或数组",
                Input::new(&inputs.nameserver_policy),
                theme,
            ))
            .child(config_input_row(
                "Hosts",
                "YAML 映射；支持单地址或地址数组",
                Input::new(&inputs.hosts),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-dns-advanced")
                        .icon(IconName::Check)
                        .label("保存 DNS 高级配置")
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.dns.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    "DNS 高级配置已保存并热重载",
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
