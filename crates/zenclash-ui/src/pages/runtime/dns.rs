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
        setting_card(zenclash_i18n::text("dns.status.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("dns.status.enable"),
                zenclash_i18n::text("dns.status.enable_description"),
                self.controlled_bool("/dns/enable", true),
                "dns-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "enable",
                        *checked,
                        zenclash_i18n::text("dns.notices.enabled"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "Fallback GeoIP",
                zenclash_i18n::text("dns.status.fallback_geoip_description"),
                self.controlled_bool("/dns/fallback-filter/geoip", true),
                "dns-fallback-geoip",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"dns": {"fallback-filter": {"geoip": *checked}}}),
                        zenclash_i18n::text("dns.notices.fallback_geoip"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("dns.status.ipv6"),
                zenclash_i18n::text("dns.status.ipv6_description"),
                self.controlled_bool("/dns/ipv6", false),
                "dns-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "ipv6",
                        *checked,
                        zenclash_i18n::text("dns.notices.ipv6"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("dns.status.use_hosts"),
                zenclash_i18n::text("dns.status.use_hosts_description"),
                self.controlled_bool("/dns/use-hosts", true),
                "dns-use-hosts",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "use-hosts",
                        *checked,
                        zenclash_i18n::text("dns.notices.hosts"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("dns.status.system_hosts"),
                zenclash_i18n::text("dns.status.system_hosts_description"),
                self.controlled_bool("/dns/use-system-hosts", true),
                "dns-use-system-hosts",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "use-system-hosts",
                        *checked,
                        zenclash_i18n::text("dns.notices.system_hosts"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("dns.status.respect_rules"),
                zenclash_i18n::text("dns.status.respect_rules_description"),
                self.controlled_bool("/dns/respect-rules", false),
                "dns-respect-rules",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_dns_bool(
                        "respect-rules",
                        *checked,
                        zenclash_i18n::text("dns.notices.rules"),
                        cx,
                    );
                }),
            ))
            .child(info_row(
                zenclash_i18n::text("dns.status.tun_hijack"),
                empty_dash(&config.tun.dns_hijack.join(", ")),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("dns.status.sniffer_mapping"),
                if config.sniffing.force_dns_mapping {
                    zenclash_i18n::text("common.status.yes")
                } else {
                    zenclash_i18n::text("common.status.no")
                },
                theme,
            ))
    }

    fn patch_dns_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: String,
        cx: &mut Context<Self>,
    ) {
        self.apply_controlled_config(json!({"dns": {key: value}}), success, cx);
    }

    fn render_dns_resolvers(&self, theme: &gpui_component::Theme) -> gpui::Div {
        let inputs = &self.config_inputs.dns;
        setting_card(zenclash_i18n::text("dns.resolvers.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.enhanced_mode"),
                "fake-ip / redir-host / normal",
                Input::new(&inputs.enhanced_mode),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.fake_ip_pool"),
                zenclash_i18n::text("dns.resolvers.fake_ip_pool_description"),
                Input::new(&inputs.fake_ip_range),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.filter_mode"),
                "blacklist / whitelist / rule",
                Input::new(&inputs.fake_ip_filter_mode),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.fake_ip_filter"),
                zenclash_i18n::text("dns.resolvers.fake_ip_filter_description"),
                Input::new(&inputs.fake_ip_filter),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.default"),
                zenclash_i18n::text("dns.resolvers.default_description"),
                Input::new(&inputs.default_nameserver),
                theme,
            ))
            .child(config_input_row(
                "Nameserver",
                zenclash_i18n::text("dns.resolvers.nameserver_description"),
                Input::new(&inputs.nameserver),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.proxy"),
                zenclash_i18n::text("dns.resolvers.proxy_description"),
                Input::new(&inputs.proxy_server_nameserver),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.direct"),
                zenclash_i18n::text("dns.resolvers.direct_description"),
                Input::new(&inputs.direct_nameserver),
                theme,
            ))
            .child(config_input_row(
                "Fallback",
                zenclash_i18n::text("dns.resolvers.fallback_description"),
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
        setting_card(zenclash_i18n::text("dns.policy.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("dns.policy.country"),
                zenclash_i18n::text("dns.policy.country_description"),
                Input::new(&inputs.fallback_geoip_code),
                theme,
            ))
            .child(config_input_row(
                "Fallback IP CIDR",
                zenclash_i18n::text("dns.policy.cidr_description"),
                Input::new(&inputs.fallback_ipcidr),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.policy.domain"),
                zenclash_i18n::text("dns.policy.domain_description"),
                Input::new(&inputs.fallback_domain),
                theme,
            ))
            .child(config_input_row(
                "Nameserver Policy",
                zenclash_i18n::text("dns.policy.nameserver_policy_description"),
                Input::new(&inputs.nameserver_policy),
                theme,
            ))
            .child(config_input_row(
                "Hosts",
                zenclash_i18n::text("dns.policy.hosts_description"),
                Input::new(&inputs.hosts),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-dns-advanced")
                        .icon(IconName::Check)
                        .label(zenclash_i18n::text("dns.policy.save"))
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.dns.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    zenclash_i18n::text("dns.notices.advanced"),
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
