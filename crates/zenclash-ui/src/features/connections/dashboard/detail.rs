use super::super::*;
use super::panel;
use gpui_kit::{InteractiveElement, TestSupportExt};

impl ConnectionsView {
    pub(super) fn render_connection_inspector(
        &self,
        projection: &projection::ConnectionProjection,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let data = &projection.snapshot;
        let selected = self
            .inspected_connection
            .as_ref()
            .and_then(|id| projection.by_id.get(id))
            .and_then(|&index| data.connections.get(index));
        let mut inspector = panel(theme)
            .id("connection-inspector")
            .test_support()
            .gap_2()
            .min_h_0()
            .w_full()
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
                    v_flex().gap_2().children(
                        [
                            (
                                "unified.connections.inbound",
                                connection.metadata.kind.clone(),
                            ),
                            ("unified.connections.host", connection.metadata.host.clone()),
                            ("unified.connections.started", connection.start.clone()),
                            (
                                "unified.connections.process",
                                connection.metadata.process.clone(),
                            ),
                            (
                                "unified.connections.path",
                                connection.metadata.process_path.clone(),
                            ),
                            ("unified.connections.id", connection.id.clone()),
                        ]
                        .into_iter()
                        .map(|(key, value)| {
                            connection_detail(
                                zenclash_i18n::text(key),
                                if value.is_empty() {
                                    "—".into()
                                } else {
                                    value
                                },
                                theme,
                            )
                        }),
                    ),
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
                            .disabled(
                                self.show_closed
                                    || !self.close_allowed
                                    || self.closing_connections.contains(&id),
                            )
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
        inspector.into_any_element()
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
