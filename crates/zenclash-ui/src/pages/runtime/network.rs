use gpui_kit::InteractiveElement;
use gpui_kit::base::TestSupportExt;
use std::collections::HashSet;

mod actions;
mod ai;
mod dialogs;
mod model;

use model::{
    format_asn, format_coordinates, format_proxy_flags, join_present, latency_color,
    public_ip_checked_at,
};

use super::{
    AppContext, Button, ButtonVariants, Context, DiagnosticData, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, Disableable, Entity, FluentBuilder, IconName, Input,
    InputState, IntoElement, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ParentElement, PublicIpProvider, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, Sizable, Styled, SystemNetworkSnapshot, Window, div, empty_dash, h_flex, json, px,
    v_flex,
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
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, system) = match &self.data {
            RuntimeData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        let snapshot = self.network_probe.snapshot.clone().unwrap_or_default();
        let steps = self
            .network_probe
            .report
            .as_ref()
            .map(|report| report.steps.as_slice())
            .unwrap_or_default();
        let passed = steps.iter().filter(|step| step.outcome.is_ok()).count();
        let elapsed = self.network_probe.report.as_ref().and_then(|report| {
            report
                .steps
                .iter()
                .map(|step| step.completed_at_ms)
                .max()
                .map(|end| end.saturating_sub(report.started_at_ms))
        });
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_6()
                    .px_4()
                    .py_3()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .bg(theme.group_box)
                    .child(
                        div()
                            .text_color(theme.success)
                            .child(zenclash_i18n::text_with(
                                "redesign.check_passed",
                                &[(
                                    "count",
                                    self.network_probe
                                        .report
                                        .as_ref()
                                        .map_or_else(|| "—".into(), |_| passed.to_string()),
                                )],
                            )),
                    )
                    .child(
                        div()
                            .text_color(theme.warning)
                            .child(zenclash_i18n::text_with(
                                "redesign.check_failed",
                                &[(
                                    "count",
                                    self.network_probe.report.as_ref().map_or_else(
                                        || "—".into(),
                                        |_| (steps.len() - passed).to_string(),
                                    ),
                                )],
                            )),
                    )
                    .child(div().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "redesign.check_elapsed",
                            &[(
                                "time",
                                elapsed.map_or_else(
                                    || "—".into(),
                                    |ms| format!("{:.1} s", ms as f64 / 1000.),
                                ),
                            )],
                        ),
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .when(compact, |row| row.flex_col())
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .when(!compact, |column| column.flex_basis(gpui_kit::rems(0.)))
                            .flex_grow(1.44)
                            .min_w_0()
                            .gap_3()
                            .child(self.render_diagnostics_card(theme, cx))
                            .child(self.render_latency_card(&snapshot, theme, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .when(!compact, |column| column.flex_basis(gpui_kit::rems(0.)))
                            .min_w_0()
                            .gap_3()
                            .child(self.render_public_ip_card(&snapshot, false, theme, cx))
                            .child(
                                self.render_system_network_card(&config, &system, false, theme, cx),
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
            zenclash_i18n::text("redesign.check_results"),
            checked_at,
            theme,
        )
        .child(
            v_flex()
                .mx_4()
                .mb_4()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .text_sm()
                        .bg(theme.table_head)
                        .text_color(theme.muted_foreground)
                        .child(
                            div()
                                .flex_1()
                                .child(zenclash_i18n::text("redesign.check_item")),
                        )
                        .child(div().w_24().child(zenclash_i18n::text("redesign.result")))
                        .child(div().w_20().child(zenclash_i18n::text("redesign.duration")))
                        .child(div().w_16().child(zenclash_i18n::text("redesign.action"))),
                )
                .children(
                    report
                        .into_iter()
                        .flat_map(|report| report.steps.iter())
                        .map(|step| self.render_diagnostic_step(step.kind, Some(step), theme, cx)),
                )
                .when(report.is_none(), |view| {
                    view.children(
                        [
                            DiagnosticStepKind::Controller,
                            DiagnosticStepKind::Capture,
                            DiagnosticStepKind::DnsA,
                            DiagnosticStepKind::DnsAaaa,
                            DiagnosticStepKind::NetworkDirect,
                            DiagnosticStepKind::NetworkMihomo,
                            DiagnosticStepKind::ProxyProviders,
                            DiagnosticStepKind::RuleProviders,
                        ]
                        .into_iter()
                        .map(|kind| self.render_diagnostic_step(kind, None, theme, cx)),
                    )
                }),
        )
    }

    fn render_public_ip_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        expanded: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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
        network_card(zenclash_i18n::text("redesign.exit_info"), theme)
            .when(!expanded, |view| {
                view.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_2()
                        .child(
                            Button::new("refresh-network-probe")
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("network.public_ip.refresh"))
                                .disabled(self.network_probe.loading || self.core_busy())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.refresh_network_probe(cx)),
                                ),
                        )
                        .child(
                            Button::new("network-options")
                                .small()
                                .ghost()
                                .label(zenclash_i18n::text("redesign.details"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_network_options(window, cx)
                                })),
                        ),
                )
            })
            .when(expanded, |card| {
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
                            .child(model::public_exit_label(
                                self.network_probe.snapshot.as_ref(),
                                self.network_probe.loading,
                            )),
                    )
                    .when_some(ip_version.filter(|_| expanded), |row, version| {
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
            .when(expanded, |view| {
                view.child(info_row(
                    zenclash_i18n::text("network.metrics.average_latency"),
                    model::average_latency(snapshot)
                        .map_or_else(|| "—".into(), |value| format!("{value} ms")),
                    theme,
                ))
            })
            .child(info_row(
                zenclash_i18n::text("network.public_ip.updated_at"),
                checked_at.unwrap_or_else(|| "—".into()),
                theme,
            ))
            .when(expanded, |this| {
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
    ) -> gpui_kit::AnyElement {
        let custom_urls = self
            .preferences
            .network_latency_targets
            .iter()
            .map(|target| target.url.as_str())
            .collect::<HashSet<_>>();
        network_card(zenclash_i18n::text("network.latency.title"), theme)
            .child(
                h_flex()
                    .px_4()
                    .pb_3()
                    .gap_2()
                    .flex_wrap()
                    .children(
                        [
                            (false, "network.diagnostics.routes.direct"),
                            (true, "network.diagnostics.routes.mihomo"),
                        ]
                        .into_iter()
                        .map(|(through_mihomo, label)| {
                            Button::new(if through_mihomo {
                                "network-route-mihomo"
                            } else {
                                "network-route-direct"
                            })
                            .outline()
                            .small()
                            .label(zenclash_i18n::text(label))
                            .selected(
                                (self.preferences.network_probe_route
                                    == NetworkProbeRoutePreference::Mihomo)
                                    == through_mihomo,
                            )
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
                                    )
                                },
                            ))
                        }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("add-network-latency-target")
                            .outline()
                            .small()
                            .label(zenclash_i18n::text("network.latency.add"))
                            .disabled(
                                self.mutation_busy(
                                    crate::pages::runtime::busy::MutationDomain::Network,
                                ) || self.preferences.network_latency_targets.len() >= 13,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_network_target(window, cx)
                            })),
                    )
                    .child(
                        Button::new("retry-network-latency")
                            .outline()
                            .small()
                            .label(zenclash_i18n::text("network.latency.retest"))
                            .loading(self.network_probe.loading)
                            .disabled(self.network_probe.loading || self.core_busy())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_network_probe(cx))),
                    ),
            )
            .child(
                v_flex()
                    .mx_4()
                    .mb_4()
                    .child(
                        h_flex()
                            .px_3()
                            .py_2()
                            .gap_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .bg(theme.table_head)
                            .child(
                                div()
                                    .w_24()
                                    .child(zenclash_i18n::text("network.latency.column_target")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(zenclash_i18n::text("network.latency.column_latency")),
                            )
                            .child(div().w_20().child(zenclash_i18n::text("redesign.status")))
                            .child(div().w_16().child(zenclash_i18n::text("redesign.action"))),
                    )
                    .when(snapshot.latencies.is_empty(), |view| {
                        view.children(
                            model::network_latency_targets(
                                &self.preferences.network_latency_targets,
                            )
                            .into_iter()
                            .map(|target| {
                                h_flex()
                                    .min_h(gpui_kit::rems(2.75))
                                    .px_3()
                                    .py_2()
                                    .gap_3()
                                    .text_sm()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div().w_24().flex_shrink_0().truncate().child(target.name),
                                    )
                                    .text_color(theme.muted_foreground)
                                    .child(div().flex_1().min_w_0().child("—"))
                                    .child(div().w_20().child("—"))
                                    .child(
                                        Button::new((
                                            gpui_kit::ElementId::from(target.url),
                                            "details",
                                        ))
                                        .small()
                                        .outline()
                                        .label(zenclash_i18n::text("redesign.details"))
                                        .disabled(true),
                                    )
                            }),
                        )
                    })
                    .children(snapshot.latencies.iter().map(|result| {
                        self.render_latency_result(
                            result,
                            custom_urls.contains(result.target.url.as_str()),
                            theme,
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_latency_result(
        &self,
        result: &zenclash_core::NetworkLatencyResult,
        custom: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let key = gpui_kit::ElementId::from(result.target.url.clone());
        let url = result.target.url.clone();
        let detail_result = result.clone();
        let color = latency_color(result.latency_ms, theme);
        h_flex()
            .min_h(gpui_kit::rems(2.75))
            .px_3()
            .py_2()
            .gap_3()
            .text_sm()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w_24()
                    .flex_shrink_0()
                    .truncate()
                    .child(result.target.name.clone()),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div().w_16().text_color(color).child(
                            result
                                .latency_ms
                                .map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
                        ),
                    )
                    .child(
                        gpui_kit::component::progress::Progress::new((key.clone(), "latency"))
                            .flex_1()
                            .h_1p5()
                            .value(
                                result
                                    .latency_ms
                                    .map_or(0., |ms| (ms as f32 / 1000. * 100.).clamp(1., 100.)),
                            )
                            .color(color),
                    ),
            )
            .child(
                h_flex()
                    .w_20()
                    .flex_shrink_0()
                    .gap_1()
                    .child(network_status_icon(result.latency_ms.is_some(), theme))
                    .child(zenclash_i18n::text(if result.latency_ms.is_some() {
                        "network.latency.succeeded"
                    } else {
                        "network.latency.failed"
                    })),
            )
            .when(result.latency_ms.is_none(), |row| {
                row.child(
                    Button::new((key.clone(), "retry"))
                        .small()
                        .outline()
                        .label(zenclash_i18n::text("common.actions.retry"))
                        .tooltip(zenclash_i18n::text("redesign.recheck_all"))
                        .disabled(self.network_probe.loading || self.core_busy())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_network_probe(cx))),
                )
            })
            .child(
                Button::new((key.clone(), "details"))
                    .outline()
                    .small()
                    .label(zenclash_i18n::text("redesign.details"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_latency_details(detail_result.clone(), window, cx)
                    })),
            )
            .when(custom, |row| {
                row.child(
                    Button::new((key, "remove"))
                        .ghost()
                        .small()
                        .icon(IconName::Delete)
                        .tooltip(zenclash_i18n::text("common.actions.delete"))
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
                            )
                        })),
                )
            })
            .into_any_element()
    }

    fn render_system_network_card(
        &self,
        config: &RuntimeConfig,
        system: &SystemNetworkSnapshot,
        expanded: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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
        network_card(zenclash_i18n::text("redesign.local_network"), theme)
            .child(network_capability_row(
                "IPv6",
                matches!(self.data, RuntimeData::Network { .. }).then_some(config.ipv6),
                theme,
            ))
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
            .when(expanded, |card| {
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
            .child(
                h_flex().px_4().py_3().gap_2().flex_wrap().children(
                    [
                        (
                            DnsCacheAction::Dns,
                            "request-dns-cache-flush",
                            "network.diagnostics.flush_dns",
                        ),
                        (
                            DnsCacheAction::FakeIp,
                            "request-fake-ip-cache-flush",
                            "network.diagnostics.flush_fake_ip",
                        ),
                    ]
                    .into_iter()
                    .map(|(action, id, label)| {
                        Button::new(id)
                            .small()
                            .outline()
                            .label(zenclash_i18n::text(label))
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_network_cache_confirmation(action, window, cx)
                            }))
                    }),
                ),
            )
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
