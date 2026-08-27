use super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement,
    RuntimePage, Styled, config_input_row, div, h_flex, json, setting_card, setting_switch, v_flex,
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
        setting_card(zenclash_i18n::text("sniffer.status.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("sniffer.status.enable"),
                zenclash_i18n::text("sniffer.status.enable_description"),
                self.controlled_bool("/sniffer/enable", current.enable),
                "sniffer-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "enable",
                        *checked,
                        zenclash_i18n::text("sniffer.notices.enabled"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("sniffer.status.dns_mapping"),
                zenclash_i18n::text("sniffer.status.dns_mapping_description"),
                self.controlled_bool("/sniffer/force-dns-mapping", current.force_dns_mapping),
                "sniffer-force-dns-mapping",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "force-dns-mapping",
                        *checked,
                        zenclash_i18n::text("sniffer.notices.dns_mapping"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("sniffer.status.pure_ip"),
                zenclash_i18n::text("sniffer.status.pure_ip_description"),
                self.controlled_bool("/sniffer/parse-pure-ip", current.parse_pure_ip),
                "sniffer-parse-pure-ip",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_sniffer_bool(
                        "parse-pure-ip",
                        *checked,
                        zenclash_i18n::text("sniffer.notices.pure_ip"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("sniffer.status.override"),
                zenclash_i18n::text("sniffer.status.override_description"),
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
                        zenclash_i18n::text("sniffer.notices.override"),
                        cx,
                    );
                }),
            ))
            .child(
                div()
                    .p_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("sniffer.status.managed_note")),
            )
    }

    fn patch_sniffer_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: String,
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
        setting_card(zenclash_i18n::text("sniffer.filters.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.http_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.http_ports),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.tls_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.tls_ports),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.quic_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.quic_ports),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.skip_domain"),
                zenclash_i18n::text("sniffer.filters.domain_description"),
                Input::new(&inputs.skip_domain),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.force_domain"),
                zenclash_i18n::text("sniffer.filters.domain_description"),
                Input::new(&inputs.force_domain),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.skip_destination"),
                zenclash_i18n::text("sniffer.filters.address_description"),
                Input::new(&inputs.skip_dst_address),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.skip_source"),
                zenclash_i18n::text("sniffer.filters.address_description"),
                Input::new(&inputs.skip_src_address),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-sniffer-advanced")
                        .icon(IconName::Check)
                        .label(zenclash_i18n::text("sniffer.filters.save"))
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let patch = this.config_inputs.sniffer.patch(cx);
                            this.apply_controlled_config(
                                patch,
                                zenclash_i18n::text("sniffer.notices.advanced"),
                                cx,
                            );
                        })),
                ),
            )
    }
}
