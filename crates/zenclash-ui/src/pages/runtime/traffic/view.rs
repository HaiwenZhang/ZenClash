use chrono::{DateTime, Local, Utc};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::{SharedString, TestSupportExt};
use zenclash_core::{TrafficDimension, TrafficOverview, TrafficTrendPoint};

use super::{TrafficHistoryFreshness, TrafficRange, dimension_label};
use crate::pages::runtime::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, InteractiveElement,
    IntoElement, PageSnapshotData, ParentElement, RuntimePage, Selectable, Sizable,
    StatefulInteractiveElement, Styled, div, empty_dash, empty_state, format_bytes,
    format_profile_age, format_speed, h_flex, metric, v_flex,
};

#[derive(Clone, Debug, PartialEq)]
struct HistoricalTrafficPoint {
    label: SharedString,
    upload: f64,
    download: f64,
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_traffic(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let realtime = self.traffic_monitor.snapshot();
        let connection_count = match self.page_observation.data() {
            PageSnapshotData::Connections(data) => Some(data.connections.len()),
            _ => None,
        };
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.upload"),
                        if realtime.connected {
                            format_speed(realtime.upload)
                        } else {
                            empty_dash("")
                        },
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.download"),
                        if realtime.connected {
                            format_speed(realtime.download)
                        } else {
                            empty_dash("")
                        },
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.range_usage"),
                        self.traffic_history.presentation().map_or_else(
                            || empty_dash(""),
                            |presentation| format_bytes(presentation.overview().totals.total),
                        ),
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.connections"),
                        connection_count.map_or_else(|| empty_dash(""), |count| count.to_string()),
                        theme,
                    )),
            )
            .child(self.render_traffic_controls(theme, cx))
            .child(self.render_traffic_signal(theme))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .flex_wrap()
                    .child(self.render_traffic_rankings(theme, cx))
                    .child(self.render_traffic_inspector(theme, cx)),
            )
            .into_any_element()
    }

    fn render_traffic_signal(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::AnyElement {
        let history = &self.traffic_history.state;
        let presentation = self.traffic_history.presentation();
        let empty = TrafficOverview::default();
        let overview = presentation.map_or(&empty, |presentation| presentation.overview());
        let range = presentation.map_or(history.range, |presentation| presentation.range());
        let points = historical_traffic_points(&overview.trend, range);
        let has_points = !points.is_empty();
        let summary = zenclash_i18n::text_with(
            "traffic.chart.summary",
            &[
                ("range", range.label()),
                ("upload", format_bytes(overview.totals.upload)),
                ("download", format_bytes(overview.totals.download)),
            ],
        );
        let (freshness, freshness_color) = match history.freshness(self.traffic_history.source()) {
            TrafficHistoryFreshness::Loading => (
                zenclash_i18n::text("traffic.chart.loading"),
                theme.muted_foreground,
            ),
            TrafficHistoryFreshness::Fresh { observed_at_ms } => (
                zenclash_i18n::text_with(
                    "traffic.chart.updated",
                    &[("age", format_profile_age(observed_at_ms / 1_000))],
                ),
                theme.success,
            ),
            TrafficHistoryFreshness::Stale { observed_at_ms } => (
                zenclash_i18n::text_with(
                    "traffic.chart.stale",
                    &[("age", format_profile_age(observed_at_ms / 1_000))],
                ),
                theme.warning,
            ),
            TrafficHistoryFreshness::Failed => {
                (zenclash_i18n::text("traffic.chart.failed"), theme.danger)
            }
        };
        let tick_margin = (points.len() / 6).max(1);
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.download)
            .stroke(theme.chart_1)
            .fill(theme.chart_1.opacity(0.18))
            .natural()
            .y(|point| point.upload)
            .stroke(theme.chart_2)
            .fill(theme.chart_2.opacity(0.14))
            .natural()
            .tick_margin(tick_margin);
        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .px_4()
                    .pt_4()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(zenclash_i18n::text("traffic.chart.title")),
                            )
                            .child(
                                div()
                                    .id("traffic-history-summary")
                                    .role(gpui_kit::Role::Status)
                                    .aria_label(summary.clone())
                                    .test_support()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(summary),
                            ),
                    )
                    .child(
                        v_flex()
                            .items_end()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(div().size_2().rounded_full().bg(freshness_color))
                                    .child(freshness),
                            )
                            .child(
                                h_flex()
                                    .gap_3()
                                    .text_xs()
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .text_color(theme.chart_2)
                                            .child(zenclash_i18n::text("traffic.chart.upload")),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .text_color(theme.chart_1)
                                            .child(zenclash_i18n::text("traffic.chart.download")),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .h(gpui_kit::rems(13.125))
                    .mx_4()
                    .mb_4()
                    .mt_3()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background.opacity(0.36))
                    .when(has_points, |this| this.child(chart))
                    .when(!has_points, |this| {
                        this.child(empty_state(
                            zenclash_i18n::text("traffic.chart.empty"),
                            theme,
                        ))
                    }),
            )
            .into_any_element()
    }

    fn render_traffic_controls(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history.state;
        h_flex()
            .gap_3()
            .flex_wrap()
            .justify_between()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_2()
                    .children(
                        TrafficRange::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, range)| {
                                Button::new(("traffic-range", index))
                                    .label(range.label())
                                    .small()
                                    .outline()
                                    .selected(history.range == range)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_traffic_range(range, cx);
                                    }))
                            }),
                    ),
            )
            .child(
                h_flex().gap_2().children(
                    [
                        TrafficDimension::Host,
                        TrafficDimension::SourceIp,
                        TrafficDimension::Outbound,
                        TrafficDimension::Process,
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, dimension)| {
                        Button::new(("traffic-dimension", index))
                            .label(dimension_label(dimension))
                            .small()
                            .outline()
                            .selected(history.dimension == dimension)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_traffic_dimension(dimension, cx);
                            }))
                    }),
                ),
            )
            .child(self.render_clear_history_control(cx))
    }

    pub(in crate::pages::runtime) fn render_clear_history_control(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        h_flex()
            .gap_2()
            .flex_wrap()
            .when(self.traffic_history.is_clear_confirmation_open(), |this| {
                this.child(
                    Button::new("cancel-clear-traffic")
                        .label(zenclash_i18n::text("traffic.actions.cancel"))
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_clear_traffic_history(cx);
                        })),
                )
                .child(
                    Button::new("confirm-clear-traffic")
                        .loading(self.mutation_busy(
                            crate::pages::runtime::busy::MutationDomain::TrafficHistory,
                        ))
                        .disabled(self.mutation_busy(
                            crate::pages::runtime::busy::MutationDomain::TrafficHistory,
                        ))
                        .icon(IconName::Globe)
                        .label(zenclash_i18n::text("traffic.actions.confirm_clear"))
                        .small()
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clear_traffic_history(cx);
                        })),
                )
            })
            .when(!self.traffic_history.is_clear_confirmation_open(), |this| {
                this.child(
                    Button::new("request-clear-traffic")
                        .icon(IconName::Globe)
                        .label(zenclash_i18n::text("traffic.actions.clear"))
                        .small()
                        .ghost()
                        .disabled(self.mutation_busy(
                            crate::pages::runtime::busy::MutationDomain::TrafficHistory,
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_clear_traffic_history(cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_traffic_rankings(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history.state;
        let presentation = self.traffic_history.presentation();
        let empty = TrafficOverview::default();
        let overview = presentation.map_or(&empty, |presentation| presentation.overview());
        let dimension =
            presentation.map_or(history.dimension, |presentation| presentation.dimension());
        let can_inspect = history.can_inspect(self.traffic_history.source());
        let ranking_title = zenclash_i18n::text_with(
            "traffic.rankings.title",
            &[("dimension", dimension_label(dimension))],
        );
        v_flex()
            .min_w(gpui_kit::rems(22.5))
            .flex_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("traffic-ranking-title")
                            .role(gpui_kit::Role::Status)
                            .aria_label(ranking_title.clone())
                            .test_support()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(ranking_title),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "traffic.rankings.samples",
                            &[("count", overview.totals.samples.to_string())],
                        ),
                    )),
            )
            .when(overview.rankings.is_empty(), |this| {
                this.child(empty_state(
                    if self.preferences.traffic_history_enabled {
                        zenclash_i18n::text("traffic.rankings.empty_enabled")
                    } else {
                        zenclash_i18n::text("traffic.rankings.empty_disabled")
                    },
                    theme,
                ))
            })
            .children(
                overview
                    .rankings
                    .iter()
                    .take(20)
                    .enumerate()
                    .map(|(index, item)| {
                        let label = item.label.clone();
                        let selected = presentation
                            .and_then(|presentation| presentation.selected_parent())
                            == Some(item.label.as_str());
                        h_flex()
                            .id(("traffic-ranking", index))
                            .min_h(gpui_kit::rems(2.75))
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                            .when(can_inspect, |row| {
                                row.cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_traffic_parent(label.clone(), cx);
                                    }))
                            })
                            .child(
                                div()
                                    .w_6()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{:02}", index + 1)),
                            )
                            .child(div().flex_1().min_w_0().text_sm().child(item.label.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(format_bytes(item.total)),
                            )
                            .child(div().text_color(theme.muted_foreground).child("›"))
                    }),
            )
    }

    fn render_traffic_inspector(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let presentation = self.traffic_history.presentation();
        let parent = presentation
            .and_then(|presentation| presentation.selected_parent())
            .map(str::to_owned);
        let detail = presentation
            .and_then(|presentation| presentation.selected_detail())
            .map(str::to_owned);
        v_flex()
            .min_w(gpui_kit::rems(20.))
            .flex_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("traffic.inspector.title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("traffic.inspector.description")),
                    ),
            )
            .when(parent.is_none(), |this| {
                this.child(empty_state(
                    zenclash_i18n::text("traffic.inspector.empty"),
                    theme,
                ))
            })
            .when_some(parent, |this, parent| {
                this.child(
                    div()
                        .px_4()
                        .py_3()
                        .text_sm()
                        .text_color(theme.primary)
                        .child(parent),
                )
                .children(self.render_traffic_details(theme, cx))
                .when_some(detail, |this, detail| {
                    this.child(
                        v_flex()
                            .mt_2()
                            .px_4()
                            .py_3()
                            .gap_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                zenclash_i18n::text_with(
                                    "traffic.inspector.actual_outbound",
                                    &[("detail", detail)],
                                ),
                            ))
                            .children(
                                presentation
                                    .map_or(&[][..], |presentation| presentation.proxy_stats())
                                    .iter()
                                    .map(|item| {
                                        h_flex()
                                            .justify_between()
                                            .child(div().text_sm().child(item.label.clone()))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .child(format_bytes(item.total)),
                                            )
                                    }),
                            ),
                    )
                })
            })
    }

    fn render_traffic_details(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let presentation = self.traffic_history.presentation();
        let can_inspect = self
            .traffic_history
            .state
            .can_select_detail(self.traffic_history.source());
        presentation
            .map_or(&[][..], |presentation| presentation.details())
            .iter()
            .take(12)
            .enumerate()
            .map(|(index, item)| {
                let label = item.label.clone();
                let selected = presentation.and_then(|presentation| presentation.selected_detail())
                    == Some(item.label.as_str());
                h_flex()
                    .id(("traffic-detail", index))
                    .min_h(gpui_kit::rems(2.5))
                    .px_4()
                    .gap_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                    .when(can_inspect, |row| {
                        row.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_traffic_detail(label.clone(), cx);
                            }))
                    })
                    .child(div().flex_1().min_w_0().text_sm().child(item.label.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format_bytes(item.total)),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

fn historical_traffic_points(
    points: &[TrafficTrendPoint],
    range: TrafficRange,
) -> Vec<HistoricalTrafficPoint> {
    points
        .iter()
        .map(|point| HistoricalTrafficPoint {
            label: format_trend_timestamp(point.timestamp_ms, range).into(),
            upload: chart_value(point.upload),
            download: chart_value(point.download),
        })
        .collect()
}

fn format_trend_timestamp(timestamp_ms: u64, range: TrafficRange) -> String {
    let timestamp_ms = i64::try_from(timestamp_ms).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.with_timezone(&Local))
        .map_or_else(
            || "—".to_owned(),
            |timestamp| match range {
                TrafficRange::Hour | TrafficRange::Day => timestamp.format("%H:%M").to_string(),
                TrafficRange::Week | TrafficRange::Month => timestamp.format("%m-%d").to_string(),
            },
        )
}

fn chart_value(bytes: u64) -> f64 {
    // The chart accepts f64; rounding above 2^53 affects display precision, not stored bytes.
    bytes as f64
}

#[cfg(test)]
mod tests;
