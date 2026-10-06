use super::*;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{
    chart::{AreaChart, PieChart},
    progress::Progress,
};
use gpui_kit::{InteractiveElement, TestSupportExt};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_connection_close_all(
        &self,
        cx: &mut Context<Self>,
    ) -> Button {
        let empty = !matches!(&self.data, RuntimeData::Connections(snapshot) if !snapshot.connections.is_empty());

        Button::new("close-all-connections")
            .icon(gpui_kit::assets::IconName::Square)
            .label(zenclash_i18n::text("connections.actions.close_all"))
            .outline()
            .danger()
            .h_10()
            .small()
            .disabled(empty || self.core_busy() || !self.connections.closing.is_empty())
            .on_click(
                cx.listener(|this, _, window, cx| this.confirm_connection_close(None, window, cx)),
            )
    }

    pub(super) fn connection_dashboard(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(projection) = &self.connections.projection else {
            return empty_state(
                zenclash_i18n::text(if self.connections.projecting {
                    "connections.filtering"
                } else {
                    "connections.empty.active"
                }),
                theme,
            )
            .into_any_element();
        };
        let data = &projection.snapshot;
        let page = list_page(
            projection.order.len(),
            self.connections.page,
            CONNECTIONS_PER_PAGE,
        );
        let selected = self
            .connections
            .expanded
            .as_ref()
            .and_then(|id| projection.by_id.get(id))
            .and_then(|&index| data.connections.get(index));
        let mut table = panel(theme)
            .id("connections-table")
            .test_support()
            .gap_0()
            .min_h(gpui_kit::rems(35.))
            .w_full()
            .min_w(gpui_kit::rems(42.))
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div().flex_1().min_w(gpui_kit::rems(12.)).child(
                            Input::new(&self.connections.filter)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search))
                                .small()
                                .h_10(),
                        ),
                    )
                    .child(self.render_connection_options(cx)),
            )
            .child(connection_columns(theme).mt_3());
        let mut rows = v_flex().gap_0().when(projection.order.is_empty(), |this| {
            this.child(empty_state(
                zenclash_i18n::text(if data.connections.is_empty() {
                    "connections.empty.active"
                } else {
                    "connections.empty.filtered"
                }),
                theme,
            ))
        });
        for &index in &projection.order[page.start..page.end] {
            let connection = &data.connections[index];
            let id = connection.id.clone();
            let detail_id = id.clone();
            let host = if connection.metadata.host.is_empty() {
                &connection.metadata.destination_ip
            } else {
                &connection.metadata.host
            };
            let active = self.connections.expanded.as_deref() == Some(id.as_str());
            rows = rows.child(
                h_flex()
                    .gap_3()
                    .min_h(gpui_kit::rems(2.875))
                    .py_1()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(active, |row| {
                        row.rounded(theme.radius)
                            .border_color(theme.table_active)
                            .bg(theme.table_active)
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .line_height(gpui_kit::relative(1.25))
                            .child(div().text_sm().truncate().child(host.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(connection.metadata.process.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .text_sm()
                            .child(connection.metadata.network.to_uppercase()),
                    )
                    .child(
                        div()
                            .w_24()
                            .flex_shrink_0()
                            .text_sm()
                            .truncate()
                            .child(connection.chains.first().cloned().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .w_20()
                            .flex_shrink_0()
                            .text_sm()
                            .text_right()
                            .child(format_bytes(connection.upload)),
                    )
                    .child(
                        div()
                            .w_20()
                            .flex_shrink_0()
                            .text_sm()
                            .text_right()
                            .child(format_bytes(connection.download)),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .text_sm()
                            .child(projection.durations[index].clone()),
                    )
                    .child(
                        Button::new(connection_element_id("connection-details", &id))
                            .label("⋯")
                            .accessibility_label(zenclash_i18n::text(
                                "connections.actions.show_details",
                            ))
                            .tooltip(zenclash_i18n::text("connections.actions.show_details"))
                            .w_8()
                            .ghost()
                            .small()
                            .bg(gpui_kit::transparent_black())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_connection_details(detail_id.clone(), cx)
                            })),
                    ),
            );
        }
        table = table.child(
            div()
                .id(("connection-table-viewport", page.index))
                .h(gpui_kit::rems(26.))
                .overflow_y_scrollbar()
                .child(rows),
        );
        table = table.child(div().flex_1()).child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .justify_between()
                .pt_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(pagination_summary(page, projection.order.len())),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("previous-connections-page")
                                .icon(IconName::ChevronLeft)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.previous_page"))
                                .disabled(page.index == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_connections_page(page.index.saturating_sub(1), cx)
                                })),
                        )
                        .child(
                            Button::new("next-connections-page")
                                .icon(IconName::ChevronRight)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.next_page"))
                                .disabled(page.index + 1 >= page.count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_connections_page(page.index + 1, cx)
                                })),
                        ),
                ),
        );
        let mut inspector = panel(theme)
            .id("connection-inspector")
            .test_support()
            .gap_2()
            .min_h(gpui_kit::rems(35.))
            .w(gpui_kit::rems(26.))
            .max_w_full()
            .flex_shrink_0()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("connections.details.title")),
            );
        if let Some(connection) = selected {
            let id = connection.id.clone();
            let host = if connection.metadata.host.is_empty() {
                &connection.metadata.destination_ip
            } else {
                &connection.metadata.host
            };
            inspector = inspector
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(endpoint_label(host, &connection.metadata.destination_port)),
                        )
                        .child(div().text_sm().text_color(theme.muted_foreground).child(
                            if connection.metadata.process.is_empty() {
                                zenclash_i18n::text("connections.unknown_process")
                            } else {
                                connection.metadata.process.clone()
                            },
                        )),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(connection_detail(
                            zenclash_i18n::text("connections.details.source"),
                            endpoint_label(
                                &connection.metadata.source_ip,
                                &connection.metadata.source_port,
                            ),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.details.destination"),
                            endpoint_label(
                                if connection.metadata.destination_ip.is_empty() {
                                    host
                                } else {
                                    &connection.metadata.destination_ip
                                },
                                &connection.metadata.destination_port,
                            ),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.columns.protocol"),
                            connection.metadata.network.clone(),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.ui.upload"),
                            format_bytes(connection.upload),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.ui.download"),
                            format_bytes(connection.download),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.columns.duration"),
                            projection
                                .by_id
                                .get(&connection.id)
                                .map(|&index| projection.durations[index].clone())
                                .unwrap_or_else(|| "—".into()),
                            theme,
                        )),
                )
                .child(
                    connection_detail_section("connections.details.rule", theme)
                        .child(connection_detail(
                            zenclash_i18n::text("connections.details.rule_type"),
                            connection.rule.clone(),
                            theme,
                        ))
                        .child(connection_detail(
                            zenclash_i18n::text("connections.details.rule_content"),
                            connection.rule_payload.clone(),
                            theme,
                        )),
                )
                .child(
                    connection_detail_section("connections.details.route", theme)
                        .child(connection_route(connection, host, theme)),
                )
                .child(
                    h_flex().child(
                        Button::new(connection_element_id("close-connection", &id))
                            .icon(gpui_kit::assets::IconName::Square)
                            .label(zenclash_i18n::text("connections.actions.close"))
                            .danger()
                            .outline()
                            .small()
                            .h_10()
                            .disabled(self.core_busy() || self.connections.closing.contains(&id))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_connection_close(Some(id.clone()), window, cx)
                            })),
                    ),
                );
        } else {
            inspector = inspector.child(empty_state(
                zenclash_i18n::text("connections.empty.details"),
                theme,
            ));
        }
        v_flex()
            .gap_3()
            .when(
                !self.core_kind.capabilities().udp_connection_tracking,
                |this| {
                    this.child(message_banner(
                        zenclash_i18n::text_with(
                            "connections.warnings.udp_tracking",
                            &[("core", self.core_kind.display_name().to_owned())],
                        ),
                        theme.warning,
                        theme,
                    ))
                },
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(connection_metric(
                        IconName::Network,
                        zenclash_i18n::text("connections.metrics.active"),
                        data.connections.len().to_string(),
                        self.connections.history.points(0),
                        theme.chart_1,
                        0,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::ArrowUp,
                        zenclash_i18n::text("connections.metrics.upload"),
                        format_bytes(data.upload_total),
                        self.connections.history.points(1),
                        theme.chart_2,
                        1,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::ArrowDown,
                        zenclash_i18n::text("connections.metrics.download"),
                        format_bytes(data.download_total),
                        self.connections.history.points(2),
                        theme.chart_1,
                        2,
                        theme,
                    ))
                    .child(connection_metric(
                        IconName::Cpu,
                        zenclash_i18n::text("connections.metrics.memory"),
                        format_bytes(data.memory),
                        self.connections.history.points(3),
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
                    .child(protocol_distribution(&projection.protocols, theme)),
            )
            .child(
                h_flex()
                    .items_stretch()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_stretch()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(42.))
                            .min_w_0()
                            .child(table)
                            .overflow_x_scrollbar(),
                    )
                    .child(inspector),
            )
            .into_any_element()
    }
}

fn connection_route(
    connection: &zenclash_core::Connection,
    target: &str,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    use gpui_kit::assets::IconName as RouteIcon;
    // Mihomo appends a selector after its selected outbound has dialed. Display
    // the reported chain in traffic direction, from outermost group to outlet.
    // https://github.com/MetaCubeX/mihomo/blob/Meta/adapter/outboundgroup/selector.go
    let mut steps = vec![(
        RouteIcon::Laptop,
        zenclash_i18n::text("connections.ui.local"),
    )];
    if connection.chains.is_empty() {
        steps.push((RouteIcon::Server, "—".into()));
    } else {
        steps.extend(
            connection
                .chains
                .iter()
                .rev()
                .map(|name| (RouteIcon::Server, name.clone())),
        );
    }
    steps.push((
        RouteIcon::Globe,
        if target.is_empty() {
            "—".into()
        } else {
            target.into()
        },
    ));
    h_flex()
        .gap_1()
        .w_full()
        .min_w_0()
        .children(steps.into_iter().enumerate().map(|(index, (icon, name))| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .when(index > 0, |step| {
                    step.child(
                        gpui_kit::component::Icon::new(IconName::ArrowRight)
                            .size_4()
                            .text_color(theme.chart_1),
                    )
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .child(
                            h_flex()
                                .size_9()
                                .justify_center()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.secondary)
                                .child(gpui_kit::component::Icon::new(icon).size_6()),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_xs()
                                .text_center()
                                .truncate()
                                .child(name),
                        ),
                )
        }))
}

fn endpoint_label(address: &str, port: &str) -> String {
    if address.is_empty() {
        return "—".into();
    }
    if port.is_empty() {
        return address.to_owned();
    }
    if address.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

fn connection_detail_section(title: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text(title)),
        )
}

fn panel(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    v_flex()
        .gap_3()
        .p_4()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .bg(theme.group_box)
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
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(zenclash_i18n::text("connections.metrics.active")),
                                ),
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

fn connection_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .child(zenclash_i18n::text("connections.columns.target")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.protocol")),
        )
        .child(
            div()
                .w_24()
                .child(zenclash_i18n::text("connections.columns.outbound")),
        )
        .child(
            div()
                .w_20()
                .text_right()
                .child(zenclash_i18n::text("connections.ui.upload")),
        )
        .child(
            div()
                .w_20()
                .text_right()
                .child(zenclash_i18n::text("connections.ui.download")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.duration")),
        )
        .child(div().w_8())
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

#[cfg(test)]
mod endpoint_tests {
    #[test]
    fn endpoint_labels_handle_missing_fields_and_ipv6() {
        assert_eq!(super::endpoint_label("", "443"), "—");
        assert_eq!(
            super::endpoint_label("example.com", "443"),
            "example.com:443"
        );
        assert_eq!(super::endpoint_label("127.0.0.1", ""), "127.0.0.1");
        assert_eq!(super::endpoint_label("::1", "443"), "[::1]:443");
    }
}
