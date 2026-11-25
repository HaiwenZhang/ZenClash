use super::super::*;
use super::panel;
use gpui_kit::component::{
    chart::{AreaChart, PieChart},
    progress::Progress,
};
use gpui_kit::{InteractiveElement, TestSupportExt};

impl ConnectionsView {
    pub(super) fn render_connection_statistics(
        &self,
        metrics: &zenclash_core::ConnectionsSnapshot,
        projection: &projection::ConnectionProjection,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .id("connection-statistics")
            .test_support()
            .gap_3()
            .flex_shrink_0()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(connection_metric(
                        IconName::Network,
                        zenclash_i18n::text("connections.metrics.active"),
                        metrics.connections.len().to_string(),
                        self.history.points(0),
                        theme.chart_1,
                        0,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::ArrowUp,
                        zenclash_i18n::text("connections.metrics.upload"),
                        format_bytes(metrics.upload_total),
                        self.history.points(1),
                        theme.chart_2,
                        1,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::ArrowDown,
                        zenclash_i18n::text("connections.metrics.download"),
                        format_bytes(metrics.download_total),
                        self.history.points(2),
                        theme.chart_1,
                        2,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::Cpu,
                        zenclash_i18n::text("connections.metrics.memory"),
                        format_bytes(metrics.memory),
                        self.history.points(3),
                        theme.chart_1,
                        3,
                        theme,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(distribution(
                        "connections.charts.processes",
                        &projection.processes,
                        theme,
                    ))
                    .child(protocol_distribution(
                        &projection.protocols,
                        self.show_closed,
                        theme,
                    )),
            )
            .into_any_element()
    }
}

fn distribution(
    title: &str,
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let maximum = values.iter().map(|value| value.1).max().unwrap_or(1).max(1);
    panel(theme)
        .flex_1()
        .flex_grow(1.65)
        .flex_basis(gpui_kit::rems(0.))
        .min_w(gpui_kit::rems(30.))
        .min_h(gpui_kit::rems(12.))
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text(title)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("connections.ui.process_hint")),
                ),
        )
        .children(values.iter().map(|(label, count)| {
            let label = if label.is_empty() {
                zenclash_i18n::text("connections.unknown_process")
            } else {
                label.clone()
            };
            h_flex()
                .gap_3()
                .child(div().w_32().text_sm().truncate().child(label.clone()))
                .child(
                    Progress::new(connection_element_id("process-share", &label))
                        .accessibility_label(label)
                        .value(*count as f32 / maximum as f32 * 100.)
                        .color(theme.chart_1)
                        .h_3()
                        .flex_1(),
                )
                .child(div().w_10().text_sm().text_right().child(count.to_string()))
        }))
}

fn protocol_distribution(
    values: &[(String, u64)],
    show_closed: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let colors = [
        theme.chart_1,
        theme.chart_1.opacity(0.65),
        theme.muted_foreground,
    ];
    let total = values.iter().map(|(_, count)| *count).sum::<u64>();
    let slices = values
        .iter()
        .enumerate()
        .map(|(index, (name, value))| (name.clone(), *value as f32, colors[index % colors.len()]))
        .collect::<Vec<_>>();
    panel(theme)
        .flex_1()
        .flex_basis(gpui_kit::rems(0.))
        .min_w(gpui_kit::rems(22.))
        .max_w_full()
        .min_h(gpui_kit::rems(12.))
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("connections.charts.protocols")),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("connections.ui.protocol_hint")),
                ),
        )
        .child(
            h_flex()
                .gap_4()
                .child(
                    div()
                        .relative()
                        .size_32()
                        .flex_shrink_0()
                        .child(
                            PieChart::new(slices)
                                .id("connection-protocol-chart")
                                .outer_radius(f32::from(theme.font_size) * 4.)
                                .inner_radius(f32::from(theme.font_size) * 2.)
                                .value(|entry| entry.1)
                                .color(|entry| entry.2)
                                .tooltip_name(|entry| entry.0.clone().into()),
                        )
                        .child(
                            v_flex()
                                .absolute()
                                .inset_0()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(total.to_string()),
                                )
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    zenclash_i18n::text(if show_closed {
                                        "unified.connections.closed"
                                    } else {
                                        "connections.metrics.active"
                                    }),
                                )),
                        ),
                )
                .child(v_flex().gap_3().children(values.iter().enumerate().map(
                    |(index, (label, count))| {
                        let percent = if total == 0 {
                            0.
                        } else {
                            *count as f64 / total as f64 * 100.
                        };
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .size_4()
                                    .rounded_full()
                                    .bg(colors[index % colors.len()]),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!("{label}   {count} ({percent:.1}%)")),
                            )
                    },
                ))),
        )
}

fn connection_metric(
    icon: IconName,
    label: String,
    value: String,
    points: Vec<(gpui_kit::SharedString, f64)>,
    color: gpui_kit::Hsla,
    index: usize,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    let maximum = points.iter().map(|point| point.1).fold(1_f64, f64::max);
    panel(theme)
        .id(("connection-metric-card", index))
        .test_support()
        .relative()
        .flex_1()
        .flex_basis(gpui_kit::rems(14.))
        .min_w_0()
        .min_h(gpui_kit::rems(7.))
        .child(
            h_flex()
                .gap_4()
                .child(
                    div()
                        .size_11()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui_kit::component::Icon::new(icon)
                                .size_6()
                                .text_color(color),
                        ),
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
                ),
        )
        .child(
            div()
                .absolute()
                .right_4()
                .bottom_2()
                .w(gpui_kit::rems(5.))
                .h(gpui_kit::rems(2.))
                .flex_shrink_0()
                .child(
                    AreaChart::new(points)
                        .id(("connection-metric-history", index))
                        .x(|point| point.0.clone())
                        .y(|point| point.1)
                        .stroke(color)
                        .fill(color.opacity(0.15))
                        .y_domain(0., maximum)
                        .natural()
                        .x_axis(false)
                        .y_axis(false)
                        .grid(false)
                        .interactive(false),
                ),
        )
}
