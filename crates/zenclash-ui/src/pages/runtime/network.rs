use std::collections::HashSet;

mod actions;
mod model;

use model::{
    average_latency, format_asn, format_coordinates, format_proxy_flags, join_present,
    latency_color,
};

use super::{
    config_input_row, div, empty_dash, h_flex, info_row, json, message_banner, metric, px,
    setting_card, setting_switch, v_flex, Button, ButtonVariants, Context, Disableable,
    FluentBuilder, IconName, Input, IntoElement, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ParentElement, PublicIpProvider, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, Sizable, Styled, SystemNetworkSnapshot,
};

#[derive(Clone, Debug, Default)]
pub(super) struct NetworkProbeUiState {
    snapshot: Option<NetworkProbeSnapshot>,
    loading: bool,
    revision: u64,
}

#[derive(Clone, Debug)]
enum NetworkPreferenceChange {
    Provider(PublicIpProvider),
    ThroughMihomo(bool),
    AddTarget(NetworkLatencyTarget),
    RemoveTarget(String),
}

impl RuntimePage {
    pub(super) fn render_network(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, system) = match &self.data {
            RuntimeData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        let snapshot = self.network_probe.snapshot.clone().unwrap_or_default();
        let average_latency = average_latency(&snapshot);
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        zenclash_i18n::text("network.metrics.public_exit"),
                        snapshot.public_ip.as_ref().map_or_else(
                            || zenclash_i18n::text("network.metrics.waiting"),
                            |info| info.ip.clone(),
                        ),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("network.metrics.average_latency"),
                        average_latency.map_or_else(|| "—".into(), |value| format!("{value} ms")),
                        latency_color(average_latency, theme),
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("network.metrics.route"),
                        empty_dash(&snapshot.route),
                        theme.warning,
                        theme,
                    )),
            )
            .child(self.render_public_ip_card(&snapshot, theme, cx))
            .child(self.render_latency_card(&snapshot, theme, cx))
            .child(self.render_system_network_card(&config, &system, theme, cx))
            .child(
                setting_card(zenclash_i18n::text("network.capabilities.title"), theme)
                    .child(info_row("IPv6", super::yes_no(config.ipv6), theme))
                    .child(info_row(
                        zenclash_i18n::text("network.capabilities.lan"),
                        super::yes_no(config.allow_lan),
                        theme,
                    ))
                    .child(info_row(
                        zenclash_i18n::text("network.capabilities.tcp_concurrent"),
                        super::yes_no(config.tcp_concurrent),
                        theme,
                    ))
                    .child(info_row(
                        zenclash_i18n::text("network.capabilities.unified_delay"),
                        super::yes_no(config.unified_delay),
                        theme,
                    )),
            )
            .into_any_element()
    }

    fn render_public_ip_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let provider = self.preferences.network_ip_provider;
        let info = snapshot.public_ip.as_ref();
        setting_card(zenclash_i18n::text("network.public_ip.title"), theme)
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(
                        h_flex().gap_2().children(
                            PublicIpProvider::ALL.into_iter().enumerate().map(
                                |(index, candidate)| {
                                    Button::new(("network-provider", index))
                                        .label(candidate.label())
                                        .small()
                                        .outline()
                                        .selected(candidate == provider)
                                        .disabled(self.mutating || self.network_probe.loading)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.persist_network_preference(
                                                NetworkPreferenceChange::Provider(candidate),
                                                zenclash_i18n::text("network.notices.provider"),
                                                cx,
                                            );
                                        }))
                                },
                            ),
                        ),
                    )
                    .child(
                        Button::new("refresh-network-probe")
                            .icon(IconName::Redo2)
                            .label(if self.network_probe.loading {
                                zenclash_i18n::text("network.public_ip.probing")
                            } else {
                                zenclash_i18n::text("network.public_ip.refresh")
                            })
                            .small()
                            .primary()
                            .disabled(self.network_probe.loading || self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_network_probe(cx);
                            })),
                    ),
            )
            .when_some(snapshot.public_ip_error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .when_some(info, |this, info| {
                this.child(info_row(
                    zenclash_i18n::text("network.public_ip.ip"),
                    &info.ip,
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.country_region"),
                    join_present(&[info.country.as_deref(), info.region.as_deref()]),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.city"),
                    info.city.as_deref().unwrap_or(""),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.organization"),
                    format_asn(info.asn, info.organization.as_deref()),
                    theme,
                ))
                .child(info_row("ISP", info.isp.as_deref().unwrap_or(""), theme))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.timezone"),
                    info.timezone.as_deref().unwrap_or(""),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.coordinates"),
                    format_coordinates(info.latitude, info.longitude),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("network.public_ip.proxy_detection"),
                    format_proxy_flags(info.is_proxy, info.is_vpn),
                    theme,
                ))
            })
    }

    fn render_latency_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let custom_urls = self
            .preferences
            .network_latency_targets
            .iter()
            .map(|target| target.url.as_str())
            .collect::<HashSet<_>>();
        setting_card(zenclash_i18n::text("network.latency.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("network.latency.through_core"),
                zenclash_i18n::text("network.latency.through_core_description"),
                self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo,
                "network-probe-through-mihomo",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.persist_network_preference(
                        NetworkPreferenceChange::ThroughMihomo(*checked),
                        zenclash_i18n::text("network.notices.route"),
                        cx,
                    );
                }),
            ))
            .children(
                snapshot
                    .latencies
                    .iter()
                    .enumerate()
                    .map(|(index, result)| {
                        self.render_latency_result(
                            index,
                            result,
                            custom_urls.contains(result.target.url.as_str()),
                            theme,
                            cx,
                        )
                    }),
            )
            .child(config_input_row(
                zenclash_i18n::text("network.latency.target_name"),
                zenclash_i18n::text("network.latency.target_name_description"),
                Input::new(&self.network_latency_name),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("network.latency.target_url"),
                zenclash_i18n::text("network.latency.target_url_description"),
                Input::new(&self.network_latency_url),
                theme,
            ))
            .child(
                h_flex().px_4().py_3().justify_end().child(
                    Button::new("add-network-latency-target")
                        .icon(IconName::Plus)
                        .label(zenclash_i18n::text("network.latency.add"))
                        .small()
                        .outline()
                        .disabled(
                            self.mutating || self.preferences.network_latency_targets.len() >= 13,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.add_network_latency_target(cx);
                        })),
                ),
            )
    }

    fn render_latency_result(
        &self,
        index: usize,
        result: &zenclash_core::NetworkLatencyResult,
        custom: bool,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let value = result.latency_ms.map_or_else(
            || {
                result
                    .error
                    .clone()
                    .unwrap_or_else(|| zenclash_i18n::text("network.latency.failed"))
            },
            |latency| format!("{latency} ms"),
        );
        let color = result
            .latency_ms
            .map_or(theme.danger, |latency| latency_color(Some(latency), theme));
        let url = result.target.url.clone();
        h_flex()
            .min_h(px(54.))
            .px_4()
            .gap_4()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(result.target.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(result.target.url.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(color).child(value))
                    .when(custom, |this| {
                        this.child(
                            Button::new(("remove-network-target", index))
                                .icon(IconName::Delete)
                                .small()
                                .ghost()
                                .disabled(self.mutating)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.persist_network_preference(
                                        NetworkPreferenceChange::RemoveTarget(url.clone()),
                                        zenclash_i18n::text("network.notices.target_removed"),
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_system_network_card(
        &self,
        config: &RuntimeConfig,
        system: &SystemNetworkSnapshot,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card(zenclash_i18n::text("network.system.title"), theme)
            .child(info_row(
                zenclash_i18n::text("network.system.interface"),
                &system.interface,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.system.gateway"),
                &system.gateway,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.system.local_address"),
                &system.local_ipv4,
                theme,
            ))
            .child(info_row("DNS", system.dns_servers.join(", "), theme))
            .when_some(system.error.clone(), |this, error| {
                this.child(message_banner(error, theme.warning, theme))
            })
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "network.system.pinned",
                            &[("interface", empty_dash(&config.interface_name))],
                        ),
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("use-system-interface")
                                    .icon(IconName::Check)
                                    .label(zenclash_i18n::text("network.system.pin"))
                                    .small()
                                    .primary()
                                    .disabled(system.interface.is_empty() || self.mutating)
                                    .on_click({
                                        let interface = system.interface.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.apply_controlled_config(
                                                json!({"interface-name": interface}),
                                                zenclash_i18n::text(
                                                    "network.notices.interface_pinned",
                                                ),
                                                cx,
                                            );
                                        })
                                    }),
                            )
                            .child(
                                Button::new("clear-system-interface")
                                    .icon(IconName::Redo2)
                                    .label(zenclash_i18n::text("network.system.automatic"))
                                    .small()
                                    .outline()
                                    .disabled(config.interface_name.is_empty() || self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.apply_controlled_config(
                                            json!({"interface-name": ""}),
                                            zenclash_i18n::text("network.notices.interface_auto"),
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
    }
}
