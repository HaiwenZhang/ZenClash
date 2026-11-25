use gpui_kit::InteractiveElement;
use gpui_kit::base::TestSupportExt;

mod actions;
mod ai;
mod diagnostic_query;
mod dialogs;
mod latency_view;
mod model;
mod probes_view;

use crate::components::page_primitives::empty_dash;
use crate::features::host::RuntimePage;
use crate::runtime_host::observation::PageSnapshotData;
use gpui_kit::AppContext;
use gpui_kit::Context;
use gpui_kit::Entity;
use gpui_kit::IntoElement;
use gpui_kit::ParentElement;
use gpui_kit::Styled;
use gpui_kit::Window;
use gpui_kit::component::Disableable;
use gpui_kit::component::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::Input;
use gpui_kit::component::input::InputState;
use gpui_kit::component::v_flex;
use gpui_kit::div;
use gpui_kit::prelude::FluentBuilder;
use zenclash_core::DiagnosticData;
use zenclash_core::DiagnosticRoute;
use zenclash_core::DiagnosticStep;
use zenclash_core::DiagnosticStepKind;
use zenclash_core::NetworkLatencyTarget;
use zenclash_core::NetworkProbeSnapshot;
use zenclash_core::PublicIpProvider;
use zenclash_core::RuntimeConfig;
use zenclash_core::SystemNetworkSnapshot;

pub(in crate::features) struct NetworkDiagnosticsUiState {
    pub(in crate::features) latency_name: Entity<InputState>,
    pub(in crate::features) latency_url: Entity<InputState>,
    pub(in crate::features) dns_name: Entity<InputState>,
    pub(in crate::features) query: diagnostic_query::NetworkDiagnosticQueryState,
    cache_confirmation: Option<DnsCacheAction>,
    adding_target: bool,
}

impl NetworkDiagnosticsUiState {
    pub(in crate::features) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> Self {
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
            query: diagnostic_query::NetworkDiagnosticQueryState::default(),
            cache_confirmation: None,
            adding_target: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::features) enum DnsCacheAction {
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
    pub(in crate::features) fn render_network(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, system) = match self.page_observation.data() {
            PageSnapshotData::Network { config, system } => (config.clone(), system.clone()),
            _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
        };
        let snapshot = self
            .network_diagnostics
            .query
            .snapshot()
            .cloned()
            .unwrap_or_default();
        let steps = self
            .network_diagnostics
            .query
            .report()
            .map(|report| report.steps.as_slice())
            .unwrap_or_default();
        let passed = steps.iter().filter(|step| step.succeeded()).count();
        let elapsed = self.network_diagnostics.query.report().and_then(|report| {
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
                                    self.network_diagnostics
                                        .query
                                        .report()
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
                                    self.network_diagnostics.query.report().map_or_else(
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
        let report = self.network_diagnostics.query.report();
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
                                .min_w_0()
                                .child(zenclash_i18n::text("redesign.check_item")),
                        )
                        .child(
                            div()
                                .w_24()
                                .flex_shrink_0()
                                .child(zenclash_i18n::text("redesign.result")),
                        )
                        .child(
                            div()
                                .w_20()
                                .flex_shrink_0()
                                .child(zenclash_i18n::text("redesign.duration")),
                        )
                        .child(
                            div()
                                .w(gpui_kit::rems(9.))
                                .flex_shrink_0()
                                .text_right()
                                .child(zenclash_i18n::text("redesign.action")),
                        ),
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
                            crate::foundation::design::online_indicator(theme)
                        } else {
                            theme.muted_foreground.opacity(0.55)
                        })
                        .text_color(theme.primary_foreground)
                        .when(enabled == Some(true), |this| {
                            this.child(gpui_kit::component::Icon::new(IconName::Check).size_4())
                        })
                        .when(enabled != Some(true), |this| {
                            this.child(
                                div()
                                    .w_2()
                                    .h(gpui_kit::rems(0.0625))
                                    .bg(theme.primary_foreground),
                            )
                        }),
                )
                .child(enabled.map_or_else(
                    || zenclash_i18n::text("common.status.unknown"),
                    crate::components::page_primitives::yes_no,
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
                        |value| crate::components::page_primitives::yes_no(value.actual.active()),
                    ),
                ),
                (
                    "tun",
                    capture.tun.value().map_or_else(
                        || zenclash_i18n::text("common.status.unknown"),
                        |value| {
                            crate::components::page_primitives::yes_no(
                                value.observed == zenclash_core::CapabilityState::Active,
                            )
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
            crate::foundation::design::online_indicator(theme)
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

#[cfg(test)]
mod ui_tests;
