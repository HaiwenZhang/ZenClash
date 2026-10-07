use super::settings::forms::{
    settings_input_row as config_input_row, settings_status, settings_switch as setting_switch,
};
use super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement,
    RuntimePage, Styled, h_flex, json, setting_card, v_flex,
};
use gpui_kit::prelude::FluentBuilder;

impl RuntimePage {
    pub(super) fn render_sniffer(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_sniffer_enable(theme, cx))
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
                            .gap_4()
                            .child(self.render_sniffer_filters(theme, cx))
                            .child(
                                setting_card(
                                    zenclash_i18n::text("settings_redesign.filters"),
                                    theme,
                                )
                                .child(self.settings_list_row(
                                    "sniffer.filters.skip_domain",
                                    &self.config_inputs.sniffer.skip_domain,
                                    cx,
                                ))
                                .child(self.settings_list_row(
                                    "sniffer.filters.force_domain",
                                    &self.config_inputs.sniffer.force_domain,
                                    cx,
                                ))
                                .child(self.settings_list_row(
                                    "sniffer.filters.skip_destination",
                                    &self.config_inputs.sniffer.skip_dst_address,
                                    cx,
                                ))
                                .child(self.settings_list_row(
                                    "sniffer.filters.skip_source",
                                    &self.config_inputs.sniffer.skip_src_address,
                                    cx,
                                )),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |view| view.w_full())
                            .child(self.render_sniffer_switches(theme, cx)),
                    ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(self.settings_cancel(cx))
                    .child(
                        Button::new("save-sniffer-advanced")
                            .icon(IconName::Check)
                            .label(zenclash_i18n::text("common.actions.save"))
                            .primary()
                            .loading(self.core_busy())
                            .disabled(self.core_busy())
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
            .into_any_element()
    }

    fn render_sniffer_enable(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let current = self.config().cloned().unwrap_or_default().sniffing;
        settings_status(
            zenclash_i18n::text("settings_redesign.sniffer_title"),
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
        )
    }

    fn render_sniffer_switches(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let current = self.config().cloned().unwrap_or_default().sniffing;
        setting_card(zenclash_i18n::text("settings_redesign.behavior"), theme)
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
        theme: &gpui_kit::component::Theme,
        _cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let inputs = &self.config_inputs.sniffer;
        setting_card(zenclash_i18n::text("settings_redesign.protocols"), theme)
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.http_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.http_ports).cleanable(true),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.tls_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.tls_ports).cleanable(true),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("sniffer.filters.quic_port"),
                zenclash_i18n::text("sniffer.filters.port_description"),
                Input::new(&inputs.quic_ports).cleanable(true),
                theme,
            ))
    }
}
