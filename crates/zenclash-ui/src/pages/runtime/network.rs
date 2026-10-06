use gpui_kit::base::TestSupportExt;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};
use std::collections::HashSet;

mod actions;
mod model;

use model::{
    average_latency, format_asn, format_coordinates, format_proxy_flags, join_present,
    latency_color, public_ip_checked_at,
};

use super::{
    AppContext, Button, ButtonVariants, Context, DiagnosticData, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, Disableable, Entity, FluentBuilder, IconName, Input,
    InputState, IntoElement, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ParentElement, PublicIpProvider, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, Sizable, Styled, SystemNetworkSnapshot, Window, config_input_row, div, empty_dash,
    h_flex, json, message_banner, px, v_flex,
};

#[derive(Debug)]
pub(super) struct NetworkProbeUiState {
    pub(super) latency_name: Entity<InputState>,
    pub(super) latency_url: Entity<InputState>,
    pub(super) dns_name: Entity<InputState>,
    pub(super) snapshot: Option<NetworkProbeSnapshot>,
    pub(super) report: Option<DiagnosticReport>,
    pub(super) loading: bool,
    pub(super) revision: u64,
    pub(super) task: super::loader::PageReadTask,
    cache_confirmation: Option<DnsCacheAction>,
    adding_target: bool,
    pub(super) details_expanded: bool,
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
            task: super::loader::PageReadTask::default(),
            cache_confirmation: None,
            adding_target: false,
            details_expanded: false,
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
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, system) = match &self.data {
            RuntimeData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        let snapshot = self.network_probe.snapshot.clone().unwrap_or_default();
        let average_latency = average_latency(&snapshot);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .flex_wrap()
                    .child(network_summary_metric(
                        gpui_kit::assets::IconName::Globe,
                        zenclash_i18n::text("network.metrics.public_exit"),
                        model::public_exit_label(
                            self.network_probe.snapshot.as_ref(),
                            self.network_probe.loading,
                        ),
                        false,
                        theme,
                    ))
                    .child(network_summary_metric(
                        gpui_kit::assets::IconName::Clock,
                        zenclash_i18n::text("network.metrics.average_latency"),
                        average_latency.map_or_else(|| "—".into(), |value| format!("{value} ms")),
                        true,
                        theme,
                    ))
                    .child(network_summary_metric(
                        gpui_kit::assets::IconName::Network,
                        zenclash_i18n::text("network.metrics.route"),
                        empty_dash(&snapshot.route),
                        true,
                        theme,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(0.))
                            .flex_grow(1.44)
                            .min_w_0()
                            .gap_3()
                            .child(self.render_diagnostics_card(theme, cx))
                            .child(self.render_latency_card(&snapshot, theme, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(0.))
                            .min_w_0()
                            .gap_3()
                            .child(self.render_public_ip_card(&snapshot, theme, cx))
                            .child(self.render_system_network_card(&config, &system, theme, cx))
                            .child(
                                network_card(
                                    zenclash_i18n::text("network.capabilities.title"),
                                    theme,
                                )
                                .child(network_capability_row("IPv6", Some(config.ipv6), theme))
                                .child(network_capability_row(
                                    zenclash_i18n::text("network.capabilities.lan"),
                                    Some(config.allow_lan),
                                    theme,
                                ))
                                .child(network_capability_row(
                                    zenclash_i18n::text("network.capabilities.tcp_concurrent"),
                                    Some(config.tcp_concurrent),
                                    theme,
                                ))
                                .child(network_capability_row(
                                    zenclash_i18n::text("network.capabilities.unified_delay"),
                                    Some(config.unified_delay),
                                    theme,
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_diagnostics_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let report = self.network_probe.report.as_ref();
        let selected_route =
            if self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo {
                DiagnosticStepKind::NetworkMihomo
            } else {
                DiagnosticStepKind::NetworkDirect
            };
        let is_primary = |step: &&DiagnosticStep| {
            matches!(
                step.kind,
                DiagnosticStepKind::Controller
                    | DiagnosticStepKind::Capture
                    | DiagnosticStepKind::DnsA
            ) || step.kind == selected_route
                || step.outcome.is_err()
        };
        let checked_at = report
            .and_then(|report| report.steps.iter().map(|step| step.completed_at_ms).max())
            .filter(|timestamp| *timestamp > 0)
            .and_then(|timestamp| i64::try_from(timestamp).ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| {
                zenclash_i18n::text_with(
                    "network.diagnostics.last_checked",
                    &[(
                        "time",
                        time.with_timezone(&chrono::Local)
                            .format("%H:%M:%S")
                            .to_string(),
                    )],
                )
            });
        network_card_with_meta(
            zenclash_i18n::text("network.diagnostics.title"),
            checked_at,
            theme,
        )
        .child(
            h_flex()
                .px_4()
                .py_3()
                .gap_3()
                .flex_wrap()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("network.diagnostics.dns_name")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(gpui_kit::rems(12.))
                        .child(Input::new(&self.network_probe.dns_name)),
                )
                .child(
                    Button::new("start-network-diagnostics")
                        .label(zenclash_i18n::text("network.diagnostics.start"))
                        .primary()
                        .small()
                        .h_10()
                        .loading(self.network_probe.loading)
                        .disabled(self.network_probe.loading || self.core_busy())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_network_probe(cx))),
                )
                .child(
                    Button::new("toggle-network-diagnostic-details")
                        .icon(if self.network_probe.details_expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .accessibility_label(zenclash_i18n::text(
                            if self.network_probe.details_expanded {
                                "network.diagnostics.hide_details"
                            } else {
                                "network.diagnostics.show_details"
                            },
                        ))
                        .tooltip(zenclash_i18n::text(
                            if self.network_probe.details_expanded {
                                "network.diagnostics.hide_details"
                            } else {
                                "network.diagnostics.show_details"
                            },
                        ))
                        .small()
                        .h_8()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network_probe.details_expanded =
                                !this.network_probe.details_expanded;
                            cx.notify();
                        })),
                ),
        )
        .child(
            v_flex()
                .mx_4()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .children(report.into_iter().flat_map(|report| {
                    report
                        .steps
                        .iter()
                        .filter(|step| self.network_probe.details_expanded || is_primary(step))
                        .map(|step| render_diagnostic_step(step, theme))
                })),
        )
        .child(
            h_flex()
                .px_4()
                .py_3()
                .gap_2()
                .flex_wrap()
                .justify_start()
                .when_some(self.network_probe.cache_confirmation, |this, action| {
                    this.child(
                        Button::new("cancel-network-cache-flush")
                            .label(zenclash_i18n::text("network.diagnostics.cancel"))
                            .small()
                            .h_10()
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
                                DnsCacheAction::FakeIp => {
                                    zenclash_i18n::text("network.diagnostics.confirm_fake_ip_flush")
                                }
                            })
                            .small()
                            .h_10()
                            .danger()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.flush_network_cache(action, cx);
                            })),
                    )
                })
                .when(self.network_probe.cache_confirmation.is_none(), |this| {
                    this.child(
                        Button::new("request-dns-cache-flush")
                            .icon(gpui_kit::component::Icon::default().path("icons/trash.svg"))
                            .label(zenclash_i18n::text("network.diagnostics.flush_dns"))
                            .small()
                            .h_10()
                            .outline()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_network_cache_flush(DnsCacheAction::Dns, cx);
                            })),
                    )
                    .child(
                        Button::new("request-fake-ip-cache-flush")
                            .icon(gpui_kit::component::Icon::default().path("icons/trash.svg"))
                            .label(zenclash_i18n::text("network.diagnostics.flush_fake_ip"))
                            .small()
                            .h_10()
                            .outline()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_network_cache_flush(DnsCacheAction::FakeIp, cx);
                            })),
                    )
                }),
        )
    }

    fn render_public_ip_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let provider = self.preferences.network_ip_provider;
        let info = snapshot.public_ip.as_ref();
        let ip_version = info
            .and_then(|info| info.ip.parse::<std::net::IpAddr>().ok())
            .map(|ip| if ip.is_ipv4() { "IPv4" } else { "IPv6" });
        let checked_at = public_ip_checked_at(snapshot, self.network_probe.report.as_ref())
            .and_then(|timestamp| i64::try_from(timestamp).ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            });
        network_card(zenclash_i18n::text("network.public_ip.title"), theme)
            .when(self.network_probe.details_expanded, |card| {
                card.child(
                    h_flex()
                        .min_h(px(58.))
                        .px_4()
                        .gap_3()
                        .justify_between()
                        .child(h_flex().gap_2().children(
                            PublicIpProvider::ALL.into_iter().enumerate().map(
                                |(index, candidate)| {
                                    Button::new(("network-provider", index))
                                    .label(candidate.label())
                                    .small()
                            .h_10()
                                    .outline()
                                    .selected(candidate == provider)
                                    .disabled(
                                        self.mutation_busy(
                                            crate::pages::runtime::busy::MutationDomain::Network,
                                        ) || self.network_probe.loading,
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.persist_network_preference(
                                            NetworkPreferenceChange::Provider(candidate),
                                            zenclash_i18n::text("network.notices.provider"),
                                            cx,
                                        );
                                    }))
                                },
                            ),
                        ))
                        .child(
                            Button::new("refresh-network-probe")
                                .icon(crate::assets::AppIcon::RefreshCw)
                                .label(if self.network_probe.loading {
                                    zenclash_i18n::text("network.public_ip.probing")
                                } else {
                                    zenclash_i18n::text("network.public_ip.refresh")
                                })
                                .small()
                                .h_10()
                                .primary()
                                .disabled(self.network_probe.loading || self.core_busy())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_network_probe(cx);
                                })),
                        ),
                )
            })
            .when_some(snapshot.public_ip_error.clone(), |this, error| {
                this.child(message_banner(error, theme.danger, theme))
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .child(
                        div()
                            .w(gpui_kit::rems(7.))
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("network.public_ip.ip")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(info.map_or_else(|| "—".into(), |info| info.ip.clone())),
                    )
                    .when_some(ip_version, |row, version| {
                        row.child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.secondary)
                                .text_sm()
                                .child(version),
                        )
                    }),
            )
            .child(info_row(
                zenclash_i18n::text("network.metrics.route"),
                empty_dash(&snapshot.route),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.public_ip.country_region"),
                info.map_or_else(
                    || "—".into(),
                    |info| {
                        join_present(&[
                            info.country.as_deref(),
                            info.region.as_deref(),
                            info.city.as_deref(),
                        ])
                    },
                ),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.public_ip.organization"),
                info.map_or_else(
                    || "—".into(),
                    |info| format_asn(info.asn, info.organization.as_deref()),
                ),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.public_ip.updated_at"),
                checked_at.unwrap_or_else(|| "—".into()),
                theme,
            ))
            .when(self.network_probe.details_expanded, |this| {
                this.when_some(info, |this, info| {
                    this.child(info_row("ISP", info.isp.as_deref().unwrap_or(""), theme))
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
            })
    }

    fn render_latency_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let custom_urls = self
            .preferences
            .network_latency_targets
            .iter()
            .map(|target| target.url.as_str())
            .collect::<HashSet<_>>();
        v_flex()
            .min_w_0()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .overflow_hidden()
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("network.latency.title")),
                    )
                    .children(
                        [
                            (true, "network.diagnostics.routes.mihomo"),
                            (false, "network.diagnostics.routes.direct"),
                        ]
                        .into_iter()
                        .map(|(through_mihomo, label)| {
                            Button::new(if through_mihomo {
                                "network-route-mihomo"
                            } else {
                                "network-route-direct"
                            })
                            .label(zenclash_i18n::text(label))
                            .small()
                            .h_10()
                            .outline()
                            .selected(
                                (self.preferences.network_probe_route
                                    == NetworkProbeRoutePreference::Mihomo)
                                    == through_mihomo,
                            )
                            .tooltip(zenclash_i18n::text(
                                "network.latency.through_core_description",
                            ))
                            .disabled(
                                self.network_probe.loading
                                    || self.mutation_busy(
                                        crate::pages::runtime::busy::MutationDomain::Network,
                                    ),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.persist_network_preference(
                                        NetworkPreferenceChange::ThroughMihomo(through_mihomo),
                                        zenclash_i18n::text("network.notices.route"),
                                        cx,
                                    );
                                },
                            ))
                        }),
                    ),
            )
            .child(
                v_flex()
                    .mx_4()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .when(!snapshot.latencies.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .px_4()
                                .py_2()
                                .gap_3()
                                .text_sm()
                                .bg(theme.table_head)
                                .text_color(theme.muted_foreground)
                                .child(
                                    div().w(gpui_kit::rems(7.)).flex_shrink_0().child(
                                        zenclash_i18n::text("network.latency.column_target"),
                                    ),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(zenclash_i18n::text(
                                        "network.latency.column_address",
                                    )),
                                )
                                .child(
                                    div().w(gpui_kit::rems(5.)).flex_shrink_0().child(
                                        zenclash_i18n::text("network.latency.column_latency"),
                                    ),
                                )
                                .child(
                                    div().w(gpui_kit::rems(7.)).flex_shrink_0().child(
                                        zenclash_i18n::text("network.latency.column_status"),
                                    ),
                                )
                                .child(div().w_8().flex_shrink_0()),
                        )
                    })
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
                    ),
            )
            .when(self.network_probe.adding_target, |this| {
                this.child(config_input_row(
                    zenclash_i18n::text("network.latency.target_name"),
                    zenclash_i18n::text("network.latency.target_name_description"),
                    Input::new(&self.network_probe.latency_name),
                    theme,
                ))
            })
            .when(self.network_probe.adding_target, |this| {
                this.child(config_input_row(
                    zenclash_i18n::text("network.latency.target_url"),
                    zenclash_i18n::text("network.latency.target_url_description"),
                    Input::new(&self.network_probe.latency_url),
                    theme,
                ))
            })
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .justify_between()
                    .when(self.network_probe.adding_target, |this| {
                        this.child(
                            Button::new("cancel-network-target")
                                .label(zenclash_i18n::text("network.diagnostics.cancel"))
                                .small()
                                .h_10()
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.network_probe.adding_target = false;
                                    this.restore_page_focus(super::Page::Network, cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("add-network-latency-target")
                            .icon(IconName::Plus)
                            .label(zenclash_i18n::text("network.latency.add"))
                            .small()
                            .h_10()
                            .outline()
                            .disabled(
                                self.mutation_busy(
                                    crate::pages::runtime::busy::MutationDomain::Network,
                                ) || self.preferences.network_latency_targets.len() >= 13,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.network_probe.adding_target {
                                    this.add_network_latency_target(cx);
                                } else {
                                    this.network_probe.adding_target = true;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("retry-network-latency")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("network.latency.retest"))
                            .small()
                            .h_10()
                            .outline()
                            .loading(self.network_probe.loading)
                            .disabled(self.network_probe.loading || self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_network_probe(cx))),
                    ),
            )
    }

    fn render_latency_result(
        &self,
        index: usize,
        result: &zenclash_core::NetworkLatencyResult,
        custom: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let value = result
            .latency_ms
            .map_or_else(|| "—".into(), |latency| format!("{latency} ms"));
        let status = if result.latency_ms.is_some() {
            zenclash_i18n::text("network.latency.succeeded")
        } else {
            result
                .error
                .clone()
                .unwrap_or_else(|| zenclash_i18n::text("network.latency.failed"))
        };
        let color = result
            .latency_ms
            .map_or(theme.danger, |latency| latency_color(Some(latency), theme));
        let url = result.target.url.clone();
        h_flex()
            .min_h(gpui_kit::rems(2.25))
            .px_4()
            .py_2()
            .gap_3()
            .text_sm()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(gpui_kit::rems(7.))
                    .flex_shrink_0()
                    .child(result.target.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .id(("latency-target-url", index))
                    .tooltip({
                        let value = result.target.url.clone();
                        move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(value.clone())
                                .build(window, cx)
                        }
                    })
                    .child(result.target.url.clone()),
            )
            .child(
                div()
                    .w(gpui_kit::rems(5.))
                    .flex_shrink_0()
                    .text_color(color)
                    .child(value),
            )
            .child(
                div()
                    .w(gpui_kit::rems(7.))
                    .flex_shrink_0()
                    .text_color(color)
                    .truncate()
                    .id(("latency-target-status", index))
                    .tooltip({
                        let value = status.clone();
                        move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(value.clone())
                                .build(window, cx)
                        }
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(network_status_icon(result.latency_ms.is_some(), theme))
                            .child(div().truncate().child(if result.latency_ms.is_some() {
                                status
                            } else {
                                zenclash_i18n::text("network.latency.failed")
                            })),
                    ),
            )
            .child(h_flex().w_8().flex_shrink_0().when(custom, |this| {
                this.child(
                    Button::new(("remove-network-target", index))
                        .icon(IconName::Delete)
                        .small()
                        .h_10()
                        .ghost()
                        .disabled(
                            self.mutation_busy(
                                crate::pages::runtime::busy::MutationDomain::Network,
                            ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.persist_network_preference(
                                NetworkPreferenceChange::RemoveTarget(url.clone()),
                                zenclash_i18n::text("network.notices.target_removed"),
                                cx,
                            );
                        })),
                )
            }))
            .into_any_element()
    }

    fn render_system_network_card(
        &self,
        config: &RuntimeConfig,
        system: &SystemNetworkSnapshot,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let capture = self.network_probe.report.as_ref().and_then(|report| {
            match report
                .step(DiagnosticStepKind::Capture)?
                .outcome
                .as_ref()
                .ok()?
            {
                DiagnosticData::Capture(capture) => Some(capture),
                _ => None,
            }
        });
        let system_proxy = capture
            .and_then(|capture| capture.system_proxy.value())
            .map(|proxy| proxy.actual.active());
        let tun = capture
            .and_then(|capture| capture.tun.value())
            .and_then(|tun| match tun.observed {
                zenclash_core::CapabilityState::Active => Some(true),
                zenclash_core::CapabilityState::Inactive => Some(false),
                _ => None,
            });
        network_card(zenclash_i18n::text("network.system.title"), theme)
            .child(network_capability_row(
                zenclash_i18n::text("tray.system_proxy"),
                system_proxy,
                theme,
            ))
            .child(network_capability_row(
                zenclash_i18n::text("home.controls.tun"),
                tun,
                theme,
            ))
            .child(info_row("DNS", system.dns_servers.join(", "), theme))
            .child(info_row(
                zenclash_i18n::text("network.system.interface"),
                &system.interface,
                theme,
            ))
            .when(self.network_probe.details_expanded, |this| {
                this.child(info_row(
                    zenclash_i18n::text("network.system.gateway"),
                    &system.gateway,
                    theme,
                ))
            })
            .child(info_row(
                zenclash_i18n::text("network.system.local_address"),
                &system.local_ipv4,
                theme,
            ))
            .when_some(system.error.clone(), |this, error| {
                this.child(message_banner(error, theme.warning, theme))
            })
            .when(self.network_probe.details_expanded, |card| {
                card.child(
                    h_flex()
                        .min_h(px(58.))
                        .px_4()
                        .gap_3()
                        .justify_between()
                        .flex_wrap()
                        .py_2()
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            zenclash_i18n::text_with(
                                "network.system.pinned",
                                &[("interface", empty_dash(&config.interface_name))],
                            ),
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("use-system-interface")
                                        .icon(IconName::Check)
                                        .label(zenclash_i18n::text("network.system.pin"))
                                        .small()
                                        .h_10()
                                        .primary()
                                        .disabled(system.interface.is_empty() || self.core_busy())
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
                                        .icon(crate::assets::AppIcon::RefreshCw)
                                        .label(zenclash_i18n::text("network.system.automatic"))
                                        .small()
                                        .h_10()
                                        .outline()
                                        .disabled(
                                            config.interface_name.is_empty() || self.core_busy(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.apply_controlled_config(
                                                json!({"interface-name": ""}),
                                                zenclash_i18n::text(
                                                    "network.notices.interface_auto",
                                                ),
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                )
            })
    }
}

fn network_capability_row(
    label: impl ToString,
    enabled: Option<bool>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .px_4()
        .py_2()
        .gap_3()
        .child(
            div()
                .w(gpui_kit::rems(7.))
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(label.to_string()),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_3()
                .text_sm()
                .text_color(if enabled == Some(true) {
                    theme.success
                } else {
                    theme.muted_foreground
                })
                .child(
                    h_flex()
                        .size_5()
                        .flex_shrink_0()
                        .justify_center()
                        .rounded_full()
                        .bg(if enabled == Some(true) {
                            theme.chart_3
                        } else {
                            theme.muted_foreground.opacity(0.55)
                        })
                        .text_color(theme.primary_foreground)
                        .when(enabled == Some(true), |this| {
                            this.child(gpui_kit::component::Icon::new(IconName::Check).size_4())
                        })
                        .when(enabled != Some(true), |this| {
                            this.child(div().w_2().h(px(1.)).bg(theme.primary_foreground))
                        }),
                )
                .child(enabled.map_or_else(
                    || zenclash_i18n::text("common.status.unknown"),
                    super::yes_no,
                )),
        )
}

fn info_row(
    label: impl ToString,
    value: impl ToString,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .px_4()
        .py_2()
        .gap_3()
        .items_start()
        .child(
            div()
                .w(gpui_kit::rems(7.))
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .child(empty_dash(&value.to_string())),
        )
}

fn render_diagnostic_step(
    step: &DiagnosticStep,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let (status, color) = match &step.outcome {
        Ok(data) => (diagnostic_data_summary(data), theme.success),
        Err(error) => (error.message.clone(), theme.danger),
    };
    h_flex()
        .min_h(px(64.))
        .px_4()
        .py_2()
        .gap_3()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .size_8()
                .flex_shrink_0()
                .justify_center()
                .rounded_full()
                .bg(theme.info.opacity(0.10))
                .text_color(theme.info)
                .child(
                    gpui_kit::component::Icon::new(match step.kind {
                        DiagnosticStepKind::Controller => {
                            gpui_kit::component::Icon::default().path("icons/server.svg")
                        }
                        DiagnosticStepKind::Capture => {
                            gpui_kit::component::Icon::new(IconName::Network)
                        }
                        _ => gpui_kit::component::Icon::new(IconName::Globe),
                    })
                    .size_5(),
                ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(diagnostic_step_label(step.kind)),
                )
                .child(div().text_sm().text_color(theme.muted_foreground).child(
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
                .flex_1()
                .min_w_0()
                .text_left()
                .text_sm()
                .text_color(color)
                .truncate()
                .id(format!("diagnostic-status:{:?}", step.kind))
                .test_support()
                .tooltip({
                    let value = status.clone();
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(value.clone()).build(window, cx)
                    }
                })
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(network_status_icon(step.outcome.is_ok(), theme))
                                .child(div().text_sm().child(zenclash_i18n::text(
                                    if step.outcome.is_ok() {
                                        "network.latency.succeeded"
                                    } else {
                                        "network.latency.failed"
                                    },
                                ))),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(status),
                        ),
                ),
        )
        .into_any_element()
}

fn network_summary_metric(
    icon: gpui_kit::assets::IconName,
    label: String,
    value: String,
    separator: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .flex_1()
        .min_w(gpui_kit::rems(14.))
        .min_h(px(104.))
        .px_4()
        .py_3()
        .gap_3()
        .when(separator, |this| {
            this.child(div().h_16().w(px(1.)).flex_shrink_0().bg(theme.border))
        })
        .child(
            h_flex()
                .size_16()
                .flex_shrink_0()
                .justify_center()
                .rounded_full()
                .bg(theme.chart_3.opacity(0.12))
                .text_color(theme.primary)
                .child(gpui_kit::component::Icon::new(icon).size_6()),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .truncate()
                        .text_2xl()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(value),
                ),
        )
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
                    capture.system_proxy.value().map_or_else(
                        || zenclash_i18n::text("common.status.unknown"),
                        |value| super::yes_no(value.actual.active()),
                    ),
                ),
                (
                    "tun",
                    capture.tun.value().map_or_else(
                        || zenclash_i18n::text("common.status.unknown"),
                        |value| {
                            super::yes_no(value.observed == zenclash_core::CapabilityState::Active)
                        },
                    ),
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
                    (
                        "ip",
                        snapshot.public_ip.as_ref().map_or_else(
                            || zenclash_i18n::text("common.status.unavailable"),
                            |info| info.ip.clone(),
                        ),
                    ),
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

fn network_card(
    title: impl Into<gpui_kit::SharedString>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    network_card_with_meta(title, None, theme)
}

fn network_card_with_meta(
    title: impl Into<gpui_kit::SharedString>,
    metadata: Option<String>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    v_flex()
        .min_w_0()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .overflow_hidden()
        .child(
            h_flex()
                .px_4()
                .py_3()
                .gap_3()
                .justify_between()
                .flex_wrap()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .when_some(metadata, |this, metadata| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(metadata),
                    )
                }),
        )
}

fn network_status_icon(succeeded: bool, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .size_4()
        .flex_shrink_0()
        .justify_center()
        .rounded_full()
        .bg(if succeeded {
            theme.chart_3
        } else {
            theme.danger
        })
        .text_color(theme.primary_foreground)
        .child(
            gpui_kit::component::Icon::new(if succeeded {
                IconName::Check
            } else {
                IconName::TriangleAlert
            })
            .size_3(),
        )
}
