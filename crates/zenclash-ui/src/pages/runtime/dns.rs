use super::settings::forms::{
    SettingsField, settings_input_row as config_input_row, settings_status,
    settings_switch as setting_switch,
};
use super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement,
    RuntimePage, Styled, h_flex, json, setting_card, v_flex,
};
use gpui_kit::prelude::FluentBuilder;

impl RuntimePage {
    pub(super) fn render_dns(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_dns_enable(theme, cx))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .when(compact, |view| view.flex_col())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |view| view.w_full())
                            .child(self.render_dns_resolvers(theme, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .when(compact, |view| view.w_full())
                            .child(self.render_dns_switches(theme, cx))
                            .child(self.render_dns_policy(theme, cx)),
                    ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(self.settings_cancel(cx))
                    .child(
                        Button::new("save-dns-advanced")
                            .icon(IconName::Check)
                            .label(zenclash_i18n::text("common.actions.save"))
                            .primary()
                            .loading(self.core_busy())
                            .disabled(self.core_busy())
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
            .into_any_element()
    }

    fn render_dns_enable(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        settings_status(
            zenclash_i18n::text("settings_redesign.dns_title"),
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
        )
    }

    fn render_dns_switches(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        setting_card(zenclash_i18n::text("settings_redesign.behavior"), theme)
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

    fn render_dns_resolvers(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let inputs = &self.config_inputs.dns;
        setting_card(zenclash_i18n::text("settings_redesign.dns_servers"), theme)
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.enhanced_mode"),
                "fake-ip / redir-host / normal",
                self.settings_choice(
                    "dns-mode",
                    &inputs.enhanced_mode,
                    &["fake-ip", "redir-host", "normal"],
                    cx,
                ),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("dns.resolvers.fake_ip_pool"),
                zenclash_i18n::text("dns.resolvers.fake_ip_pool_description"),
                Input::new(&inputs.fake_ip_range).cleanable(true),
                theme,
            ))
            .child(self.settings_list_row("dns.resolvers.default", &inputs.default_nameserver, cx))
            .child(self.settings_list_row(
                "settings_redesign.dns_nameserver",
                &inputs.nameserver,
                cx,
            ))
            .child(self.settings_list_row(
                "dns.resolvers.proxy",
                &inputs.proxy_server_nameserver,
                cx,
            ))
            .child(self.settings_list_row("dns.resolvers.direct", &inputs.direct_nameserver, cx))
            .child(self.settings_list_row("settings_redesign.dns_fallback", &inputs.fallback, cx))
    }

    fn render_dns_policy(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let inputs = &self.config_inputs.dns;
        setting_card(zenclash_i18n::text("settings_redesign.dns_policy"), theme)
            .child(
                self.settings_group_row(
                    "dns.resolvers.fake_ip_filter",
                    inputs
                        .fake_ip_filter
                        .read(cx)
                        .value()
                        .lines()
                        .count()
                        .to_string(),
                    vec![
                        SettingsField {
                            label: "dns.resolvers.filter_mode",
                            input: inputs.fake_ip_filter_mode.clone().into(),
                            choices: &["blacklist", "whitelist", "rule"],
                        },
                        SettingsField {
                            label: "dns.resolvers.fake_ip_filter",
                            input: inputs.fake_ip_filter.clone().into(),
                            choices: &[],
                        },
                    ],
                    cx,
                ),
            )
            .child(self.settings_list_row(
                "settings_redesign.dns_mapping",
                &inputs.nameserver_policy,
                cx,
            ))
            .child(self.settings_list_row("settings_redesign.dns_hosts", &inputs.hosts, cx))
            .child(self.settings_group_row(
                "settings_redesign.fallback_filter",
                inputs.fallback_geoip_code.read(cx).value().to_string(),
                vec![
                    SettingsField {
                        label: "settings_redesign.geoip_filter",
                        input: inputs.fallback_geoip.clone().into(),
                        choices: &["true", "false"],
                    },
                    SettingsField {
                        label: "dns.policy.country",
                        input: inputs.fallback_geoip_code.clone().into(),
                        choices: &[],
                    },
                    SettingsField {
                        label: "settings_redesign.dns_cidr",
                        input: inputs.fallback_ipcidr.clone().into(),
                        choices: &[],
                    },
                    SettingsField {
                        label: "dns.policy.domain",
                        input: inputs.fallback_domain.clone().into(),
                        choices: &[],
                    },
                ],
                cx,
            ))
    }
}
