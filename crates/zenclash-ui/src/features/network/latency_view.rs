use std::collections::HashSet;

use gpui_kit::component::Selectable;
use zenclash_core::NetworkProbeRoutePreference;

use super::model::latency_color;
use super::*;

impl RuntimePage {
    pub(super) fn render_latency_card(
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
                                self.network_diagnostics.query.is_loading()
                                    || self.mutation_busy(
                                        crate::runtime_host::busy::MutationDomain::Network,
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
                                    crate::runtime_host::busy::MutationDomain::Network,
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
                            .loading(self.network_diagnostics.query.is_loading())
                            .disabled(
                                self.network_diagnostics.query.is_loading() || self.core_busy(),
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.refresh_network_diagnostics(cx)),
                            ),
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
                                    .flex_shrink_0()
                                    .child(zenclash_i18n::text("network.latency.column_target")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(zenclash_i18n::text("network.latency.column_latency")),
                            )
                            .child(
                                div()
                                    .w_20()
                                    .flex_shrink_0()
                                    .child(zenclash_i18n::text("redesign.status")),
                            )
                            .child(
                                div()
                                    .w(gpui_kit::rems(10.))
                                    .flex_shrink_0()
                                    .text_right()
                                    .child(zenclash_i18n::text("redesign.action")),
                            ),
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
                                    .child(div().w_20().flex_shrink_0().child("—"))
                                    .child(
                                        h_flex()
                                            .w(gpui_kit::rems(10.))
                                            .flex_shrink_0()
                                            .justify_end()
                                            .child(
                                                Button::new((
                                                    gpui_kit::ElementId::from(target.url),
                                                    "details",
                                                ))
                                                .small()
                                                .outline()
                                                .label(zenclash_i18n::text("redesign.details"))
                                                .disabled(true),
                                            ),
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
            .child(
                h_flex()
                    .w(gpui_kit::rems(10.))
                    .flex_shrink_0()
                    .justify_end()
                    .gap_2()
                    .when(result.latency_ms.is_none(), |row| {
                        row.child(
                            Button::new((key.clone(), "retry"))
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.retry"))
                                .tooltip(zenclash_i18n::text("redesign.recheck_all"))
                                .disabled(
                                    self.network_diagnostics.query.is_loading() || self.core_busy(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_network_diagnostics(cx)
                                })),
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
                                .accessibility_label(zenclash_i18n::text("common.actions.delete"))
                                .tooltip(zenclash_i18n::text("common.actions.delete"))
                                .disabled(self.mutation_busy(
                                    crate::runtime_host::busy::MutationDomain::Network,
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.persist_network_preference(
                                        NetworkPreferenceChange::RemoveTarget(url.clone()),
                                        zenclash_i18n::text("network.notices.target_removed"),
                                        cx,
                                    )
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}
