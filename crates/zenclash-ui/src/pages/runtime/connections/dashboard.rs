use super::*;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Selectable, chart::PieChart, progress::Progress};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_connection_close_all(
        &self,
        cx: &mut Context<Self>,
    ) -> Button {
        let empty = !matches!(&self.data, RuntimeData::Connections(snapshot) if !snapshot.connections.is_empty());

        Button::new("close-all-connections")
            .icon(IconName::CircleX)
            .label(zenclash_i18n::text("connections.actions.close_all"))
            .danger()
            .small()
            .disabled(empty || self.core_busy() || !self.connections.closing.is_empty())
            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx)))
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
        let projection_pending =
            projection.query != self.connections.query || self.connections.projecting;
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
            .gap_0p5()
            .w_full()
            .min_w(gpui_kit::rems(42.))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.connections.filter)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search))
                                .small(),
                        ),
                    )
                    .child(self.render_connection_options(cx)),
            )
            .child(connection_columns(theme))
            .when(projection.order.is_empty(), |this| {
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
            table = table.child(
                h_flex()
                    .gap_3()
                    .py_1()
                    .px_2()
                    .rounded(theme.radius)
                    .border_b_1()
                    .border_color(theme.border)
                    .when(active, |row| row.bg(theme.primary.opacity(0.1)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(host.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(connection.metadata.process.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .text_xs()
                            .child(connection.metadata.network.clone()),
                    )
                    .child(
                        div()
                            .w_24()
                            .text_xs()
                            .truncate()
                            .child(connection.chains.first().cloned().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .w_20()
                            .text_xs()
                            .text_right()
                            .child(format_bytes(connection.upload)),
                    )
                    .child(
                        div()
                            .w_20()
                            .text_xs()
                            .text_right()
                            .child(format_bytes(connection.download)),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .text_xs()
                            .child(projection.durations[index].clone()),
                    )
                    .child(
                        Button::new(connection_element_id("connection-details", &id))
                            .icon(IconName::Eye)
                            .accessibility_label(zenclash_i18n::text(
                                "connections.actions.show_details",
                            ))
                            .tooltip(zenclash_i18n::text("connections.actions.show_details"))
                            .w_8()
                            .ghost()
                            .small()
                            .selected(active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_connection_details(detail_id.clone(), cx)
                            })),
                    ),
            );
        }
        table = table.child(
            h_flex()
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
        let mut inspector = panel(theme).w_80().flex_shrink_0().child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text("connections.details.title")),
        );
        if let Some(connection) = selected {
            let id = connection.id.clone();
            inspector = inspector
                .child(div().text_sm().child(format!(
                    "{}:{}",
                    connection.metadata.host, connection.metadata.destination_port
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(connection.metadata.process.clone()),
                )
                .child(connection_detail(
                    zenclash_i18n::text("connections.details.source"),
                    format!(
                        "{}:{}",
                        connection.metadata.source_ip, connection.metadata.source_port
                    ),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.details.destination"),
                    format!(
                        "{}:{}",
                        connection.metadata.destination_ip, connection.metadata.destination_port
                    ),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.columns.protocol"),
                    connection.metadata.network.clone(),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.metrics.upload"),
                    format_bytes(connection.upload),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.metrics.download"),
                    format_bytes(connection.download),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.columns.started"),
                    connection.start.clone(),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.details.rule"),
                    format!("{} · {}", connection.rule, connection.rule_payload),
                    theme,
                ))
                .child(connection_detail(
                    zenclash_i18n::text("connections.details.route"),
                    connection.chains.join(" → "),
                    theme,
                ))
                .child(
                    Button::new(connection_element_id("close-connection", &id))
                        .icon(IconName::CircleX)
                        .label(zenclash_i18n::text("connections.actions.close"))
                        .danger()
                        .small()
                        .disabled(self.core_busy() || self.connections.closing.contains(&id))
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.close_connection(id.clone(), cx)
                            }),
                        ),
                );
        } else {
            inspector = inspector.child(empty_state(
                zenclash_i18n::text("connections.empty.details"),
                theme,
            ));
        }
        v_flex()
            .gap_4()
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
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.active"),
                        data.connections.len().to_string(),
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.upload"),
                        format_bytes(data.upload_total),
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.download"),
                        format_bytes(data.download_total),
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.memory"),
                        format_bytes(data.memory),
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
                h_flex().justify_between().child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text(if projection_pending {
                            "connections.filtering"
                        } else {
                            "connections.refresh_hint"
                        })),
                ),
            )
            .child(
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
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
        .flex_basis(gpui_kit::rems(30.))
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text("runtime.charts.top_five")),
        )
        .children(values.iter().map(|(label, count)| {
            let label = if label.is_empty() {
                zenclash_i18n::text("connections.unknown_process")
            } else {
                label.clone()
            };
            h_flex()
                .gap_3()
                .child(div().w_32().text_xs().truncate().child(label.clone()))
                .child(
                    Progress::new(connection_element_id("process-share", &label))
                        .accessibility_label(label)
                        .value(*count as f32 / maximum as f32 * 100.)
                        .color(theme.info)
                        .flex_1(),
                )
                .child(div().w_10().text_xs().text_right().child(count.to_string()))
        }))
}

fn protocol_distribution(
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let colors = [theme.info, theme.primary, theme.muted_foreground];
    let slices = values
        .iter()
        .enumerate()
        .map(|(index, (name, value))| (name.clone(), *value as f32, colors[index % colors.len()]))
        .collect::<Vec<_>>();
    panel(theme)
        .w_80()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text("connections.charts.protocols")),
        )
        .child(
            h_flex()
                .gap_3()
                .child(
                    div().size_32().child(
                        PieChart::new(slices)
                            .id("connection-protocol-chart")
                            .inner_radius(f32::from(theme.font_size) * 2.5)
                            .value(|entry| entry.1)
                            .color(|entry| entry.2)
                            .tooltip_name(|entry| entry.0.clone().into()),
                    ),
                )
                .child(
                    v_flex().gap_3().children(
                        values.iter().map(|(label, count)| {
                            div().text_xs().child(format!("{label}   {count}"))
                        }),
                    ),
                ),
        )
}

fn connection_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .rounded(theme.radius)
        .text_xs()
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
                .child(zenclash_i18n::text("connections.metrics.upload")),
        )
        .child(
            div()
                .w_20()
                .text_right()
                .child(zenclash_i18n::text("connections.metrics.download")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.duration")),
        )
        .child(div().w_8())
}
