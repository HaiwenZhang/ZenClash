use chrono::{DateTime, Local, Utc};
use gpui::SharedString;
use gpui_component::chart::AreaChart;
use zenclash_core::{TrafficDimension, TrafficTrendPoint};

use super::{TrafficRange, dimension_label};
use crate::pages::runtime::{
    Button, ButtonVariants, ConnectionsSnapshot, Context, Disableable, FluentBuilder, IconName,
    InteractiveElement, IntoElement, ParentElement, RuntimeData, RuntimePage, Selectable, Sizable,
    StatefulInteractiveElement, Styled, div, empty_state, format_bytes, format_speed, h_flex,
    metric, px, v_flex,
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
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let realtime = self.traffic_monitor.snapshot();
        let connections = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let history = &self.traffic_history;
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.upload"),
                        format_speed(realtime.upload),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.download"),
                        format_speed(realtime.download),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.range_usage"),
                        format_bytes(history.overview.totals.total),
                        theme.warning,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("traffic.metrics.connections"),
                        connections.connections.len().to_string(),
                        theme.foreground,
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

    fn render_traffic_signal(&self, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let history = &self.traffic_history;
        let points = historical_traffic_points(&history.overview.trend, history.range);
        let has_points = !points.is_empty();
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
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(zenclash_i18n::text("traffic.chart.title")),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                zenclash_i18n::text_with(
                                    "traffic.chart.summary",
                                    &[
                                        ("range", history.range.label()),
                                        ("upload", format_bytes(history.overview.totals.upload)),
                                        (
                                            "download",
                                            format_bytes(history.overview.totals.download),
                                        ),
                                    ],
                                ),
                            )),
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
            )
            .child(
                div()
                    .h(px(210.))
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
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
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

    fn render_clear_history_control(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        h_flex()
            .gap_2()
            .when(self.traffic_history.clear_confirmation, |this| {
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
                        .icon(IconName::Delete)
                        .label(zenclash_i18n::text("traffic.actions.confirm_clear"))
                        .small()
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clear_traffic_history(cx);
                        })),
                )
            })
            .when(!self.traffic_history.clear_confirmation, |this| {
                this.child(
                    Button::new("request-clear-traffic")
                        .icon(IconName::Delete)
                        .label(zenclash_i18n::text("traffic.actions.clear"))
                        .small()
                        .ghost()
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_clear_traffic_history(cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_traffic_rankings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
        v_flex()
            .min_w(px(360.))
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
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text_with(
                                "traffic.rankings.title",
                                &[("dimension", dimension_label(history.dimension))],
                            )),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "traffic.rankings.samples",
                            &[("count", history.overview.totals.samples.to_string())],
                        ),
                    )),
            )
            .when(history.overview.rankings.is_empty(), |this| {
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
                history
                    .overview
                    .rankings
                    .iter()
                    .take(20)
                    .enumerate()
                    .map(|(index, item)| {
                        let label = item.label.clone();
                        let selected = history.selected_parent.as_deref() == Some(&item.label);
                        h_flex()
                            .id(("traffic-ranking", index))
                            .min_h(px(44.))
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_traffic_parent(label.clone(), cx);
                            }))
                            .child(
                                div()
                                    .w(px(24.))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{:02}", index + 1)),
                            )
                            .child(div().flex_1().min_w_0().text_sm().child(item.label.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(format_bytes(item.total)),
                            )
                            .child(div().text_color(theme.muted_foreground).child("›"))
                    }),
            )
    }

    fn render_traffic_inspector(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &self.traffic_history;
        v_flex()
            .min_w(px(320.))
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("traffic.inspector.title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("traffic.inspector.description")),
                    ),
            )
            .when(history.selected_parent.is_none(), |this| {
                this.child(empty_state(
                    zenclash_i18n::text("traffic.inspector.empty"),
                    theme,
                ))
            })
            .when_some(history.selected_parent.clone(), |this, parent| {
                this.child(
                    div()
                        .px_4()
                        .py_3()
                        .text_sm()
                        .text_color(theme.primary)
                        .child(parent),
                )
                .children(self.render_traffic_details(theme, cx))
                .when_some(history.selected_detail.clone(), |this, detail| {
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
                            .children(history.proxy_stats.iter().map(|item| {
                                h_flex()
                                    .justify_between()
                                    .child(div().text_sm().child(item.label.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(format_bytes(item.total)),
                                    )
                            })),
                    )
                })
            })
    }

    fn render_traffic_details(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        self.traffic_history
            .details
            .iter()
            .take(12)
            .enumerate()
            .map(|(index, item)| {
                let label = item.label.clone();
                let selected = self.traffic_history.selected_detail.as_deref() == Some(&item.label);
                h_flex()
                    .id(("traffic-detail", index))
                    .min_h(px(40.))
                    .px_4()
                    .gap_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(selected, |this| this.bg(theme.primary.opacity(0.1)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_traffic_detail(label.clone(), cx);
                    }))
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
    f64::from(u32::try_from(bytes).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_chart_keeps_upload_and_download_as_distinct_series() {
        let points = historical_traffic_points(
            &[TrafficTrendPoint {
                timestamp_ms: 1_700_000_000_000,
                upload: 1_024,
                download: 8_192,
            }],
            TrafficRange::Hour,
        );

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].upload, 1_024.0);
        assert_eq!(points[0].download, 8_192.0);
        assert!(!points[0].label.is_empty());
    }

    #[test]
    fn history_chart_caps_values_that_exceed_chart_numeric_support() {
        assert_eq!(chart_value(u64::MAX), f64::from(u32::MAX));
    }
}
