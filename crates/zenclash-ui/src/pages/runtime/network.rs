use std::collections::HashSet;

mod actions;
mod model;

use model::{
    average_latency, format_asn, format_coordinates, format_proxy_flags, join_present,
    latency_color,
};

use super::{
    AppContext, Button, ButtonVariants, Context, DiagnosticData, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, Disableable, Entity, FluentBuilder, IconName, Input,
    InputState, IntoElement, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ParentElement, PublicIpProvider, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, Sizable, Styled, SystemNetworkSnapshot, Window, config_input_row, div, empty_dash,
    h_flex, info_row, json, message_banner, metric, px, setting_card, setting_switch, v_flex,
};

#[derive(Clone, Debug)]
pub(super) struct NetworkProbeUiState {
    pub(super) latency_name: Entity<InputState>,
    pub(super) latency_url: Entity<InputState>,
    pub(super) dns_name: Entity<InputState>,
    snapshot: Option<NetworkProbeSnapshot>,
    report: Option<DiagnosticReport>,
    loading: bool,
    revision: u64,
    cache_confirmation: Option<DnsCacheAction>,
}

impl NetworkProbeUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> Self {
        Self {
            latency_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(zenclash_i18n::text("runtime.placeholders.network_target"))
            }),
            latency_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("https://example.com/generate_204")
            }),
            dns_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("example.com")
                    .placeholder(zenclash_i18n::text("runtime.placeholders.dns_name"))
            }),
            snapshot: None,
            report: None,
            loading: false,
            revision: 0,
            cache_confirmation: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DnsCacheAction {
    Dns,
    FakeIp,
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
            .child(self.render_diagnostics_card(theme, cx))
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

    fn render_diagnostics_card(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let report = self.network_probe.report.as_ref();
        setting_card(zenclash_i18n::text("network.diagnostics.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("network.diagnostics.dns_name"),
                zenclash_i18n::text("network.diagnostics.dns_name_description"),
                Input::new(&self.network_probe.dns_name),
                theme,
            ))
            .children(report.into_iter().flat_map(|report| {
                report
                    .steps
                    .iter()
                    .map(|step| render_diagnostic_step(step, theme))
            }))
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .flex_wrap()
                    .justify_end()
                    .when_some(self.network_probe.cache_confirmation, |this, action| {
                        this.child(
                            Button::new("cancel-network-cache-flush")
                                .label(zenclash_i18n::text("network.diagnostics.cancel"))
                                .small()
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_network_cache_flush(cx);
                                })),
                        )
                        .child(
                            Button::new("confirm-network-cache-flush")
                                .icon(IconName::Delete)
                                .label(match action {
                                    DnsCacheAction::Dns => {
                                        zenclash_i18n::text("network.diagnostics.confirm_dns_flush")
                                    }
                                    DnsCacheAction::FakeIp => zenclash_i18n::text(
                                        "network.diagnostics.confirm_fake_ip_flush",
                                    ),
                                })
                                .small()
                                .danger()
                                .disabled(self.mutating)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.flush_network_cache(action, cx);
                                })),
                        )
                    })
                    .when(self.network_probe.cache_confirmation.is_none(), |this| {
                        this.child(
                            Button::new("request-dns-cache-flush")
                                .label(zenclash_i18n::text("network.diagnostics.flush_dns"))
                                .small()
                                .outline()
                                .disabled(self.mutating)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_network_cache_flush(DnsCacheAction::Dns, cx);
                                })),
                        )
                        .child(
                            Button::new("request-fake-ip-cache-flush")
                                .label(zenclash_i18n::text("network.diagnostics.flush_fake_ip"))
                                .small()
                                .outline()
                                .disabled(self.mutating)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_network_cache_flush(DnsCacheAction::FakeIp, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("copy-support-bundle")
                            .icon(IconName::Copy)
                            .label(zenclash_i18n::text("network.diagnostics.copy_support"))
                            .small()
                            .primary()
                            .disabled(report.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.copy_network_support_bundle(cx);
                            })),
                    ),
            )
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
                Input::new(&self.network_probe.latency_name),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("network.latency.target_url"),
                zenclash_i18n::text("network.latency.target_url_description"),
                Input::new(&self.network_probe.latency_url),
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

fn render_diagnostic_step(
    step: &DiagnosticStep,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let (status, color) = match &step.outcome {
        Ok(data) => (diagnostic_data_summary(data), theme.success),
        Err(error) => (error.message.clone(), theme.danger),
    };
    h_flex()
        .min_h(px(54.))
        .px_4()
        .gap_3()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex()
                .gap_1()
                .child(div().text_sm().child(diagnostic_step_label(step.kind)))
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    zenclash_i18n::text_with(
                        "network.diagnostics.route_time",
                        &[
                            ("route", diagnostic_route_label(step.route)),
                            ("duration", step.duration_ms.to_string()),
                        ],
                    ),
                )),
        )
        .child(
            div()
                .max_w(px(620.))
                .text_right()
                .text_xs()
                .text_color(color)
                .child(status),
        )
        .into_any_element()
}

fn diagnostic_step_label(kind: DiagnosticStepKind) -> String {
    let key = match kind {
        DiagnosticStepKind::Controller => "network.diagnostics.steps.controller",
        DiagnosticStepKind::Capture => "network.diagnostics.steps.capture",
        DiagnosticStepKind::DnsA => "network.diagnostics.steps.dns_a",
        DiagnosticStepKind::DnsAaaa => "network.diagnostics.steps.dns_aaaa",
        DiagnosticStepKind::NetworkDirect => "network.diagnostics.steps.direct",
        DiagnosticStepKind::NetworkMihomo => "network.diagnostics.steps.mihomo",
        DiagnosticStepKind::ProxyProviders => "network.diagnostics.steps.proxy_providers",
        DiagnosticStepKind::RuleProviders => "network.diagnostics.steps.rule_providers",
    };
    zenclash_i18n::text(key)
}

fn diagnostic_route_label(route: DiagnosticRoute) -> String {
    let key = match route {
        DiagnosticRoute::Controller => "network.diagnostics.routes.controller",
        DiagnosticRoute::Local => "network.diagnostics.routes.local",
        DiagnosticRoute::Direct => "network.diagnostics.routes.direct",
        DiagnosticRoute::Mihomo => "network.diagnostics.routes.mihomo",
    };
    zenclash_i18n::text(key)
}

fn diagnostic_data_summary(data: &DiagnosticData) -> String {
    match data {
        DiagnosticData::Controller(version) => zenclash_i18n::text_with(
            "network.diagnostics.results.controller",
            &[("version", empty_dash(&version.version))],
        ),
        DiagnosticData::Capture(capture) => zenclash_i18n::text_with(
            "network.diagnostics.results.capture",
            &[
                (
                    "system_proxy",
                    super::yes_no(
                        capture
                            .system_proxy
                            .value()
                            .is_some_and(|value| value.actual.active()),
                    ),
                ),
                (
                    "tun",
                    super::yes_no(capture.tun.value().is_some_and(|value| {
                        value.observed == zenclash_core::CapabilityState::Active
                    })),
                ),
            ],
        ),
        DiagnosticData::Dns(response) => {
            let answers = response
                .answer
                .iter()
                .map(|answer| format!("{} (TTL {}s)", answer.data, answer.ttl))
                .collect::<Vec<_>>()
                .join(", ");
            zenclash_i18n::text_with(
                "network.diagnostics.results.dns",
                &[
                    ("status", response.status.to_string()),
                    ("count", response.answer.len().to_string()),
                    ("answers", empty_dash(&answers)),
                ],
            )
        }
        DiagnosticData::Network(snapshot) => {
            let succeeded = snapshot
                .latencies
                .iter()
                .filter(|result| result.latency_ms.is_some())
                .count();
            zenclash_i18n::text_with(
                "network.diagnostics.results.network",
                &[
                    ("ip", super::yes_no(snapshot.public_ip.is_some())),
                    ("success", succeeded.to_string()),
                    ("total", snapshot.latencies.len().to_string()),
                ],
            )
        }
        DiagnosticData::Providers(catalog) => zenclash_i18n::text_with(
            "network.diagnostics.results.providers",
            &[("count", catalog.providers.len().to_string())],
        ),
    }
}
