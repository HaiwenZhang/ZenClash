use std::collections::HashSet;

use super::{
    AppContext, Button, ButtonVariants, Context, Disableable, Entity, FluentBuilder, Icon,
    IconName, Input, InputEvent, InputState, InteractiveElement, IntoElement, Page, ParentElement,
    RuntimeData, RuntimePage, Sizable, Styled, Subscription, Window,
    contains_ascii_case_insensitive, div, empty_state, format_bytes, h_flex, list_page,
    message_banner, metric, pagination_summary, px, v_flex,
};

const CONNECTIONS_PER_PAGE: usize = 100;

pub(super) struct ConnectionsUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) closing: HashSet<String>,
    pub(super) expanded: Option<String>,
    pub(super) page: usize,
}

impl ConnectionsUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text(
                "runtime.placeholders.connection_filter",
            ))
        });
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.connections.page = 0;
                cx.notify();
            }
        });
        (
            Self {
                filter,
                closing: HashSet::new(),
                expanded: None,
                page: 0,
            },
            subscription,
        )
    }
}

impl RuntimePage {
    fn toggle_connection_details(&mut self, id: String, cx: &mut Context<Self>) {
        self.connections.expanded = if self.connections.expanded.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    fn set_connections_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.connections.page = page;
        cx.notify();
    }

    fn close_all_connections(&mut self, cx: &mut Context<Self>) {
        if !self.connections.closing.is_empty() {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Connections) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_all_connections()
                .await
                .map_err(|error| error.to_string())?;
            client
                .connections_snapshot()
                .await
                .map(RuntimeData::Connections)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice =
                                Some(zenclash_i18n::text("connections.notices.closed_all"));
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_connection(&mut self, id: String, cx: &mut Context<Self>) {
        if self.mutating || !self.connections.closing.insert(id.clone()) {
            return;
        }
        self.invalidate_page_load();
        self.error = None;
        let token = self.page_task_token_for(Page::Connections);
        let client = self.client.clone();
        let id_for_task = id.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_connection(&id_for_task)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.connections.closing.remove(&id);
                match result {
                    Ok(()) if this.is_page_task_current(token) => this.refresh(cx),
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the connection page is a cohesive declarative GPUI element tree"
    )]
    pub(super) fn render_connections(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let data = match &self.data {
            RuntimeData::Connections(data) => data,
            _ => {
                return empty_state(zenclash_i18n::text("connections.empty.active"), theme);
            }
        };
        let total = data.connections.len();
        let query = normalize_connection_query(&self.connections.filter.read(cx).value());
        let visible = data
            .connections
            .iter()
            .filter(|connection| connection_matches(connection, &query))
            .count();
        let page = list_page(visible, self.connections.page, CONNECTIONS_PER_PAGE);
        let filtered = data
            .connections
            .iter()
            .filter(|connection| connection_matches(connection, &query))
            .skip(page.start)
            .take(page.end - page.start)
            .collect::<Vec<_>>();
        let previous_page = page.index.saturating_sub(1);
        let next_page = page.index + 1;
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
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.active"),
                        total.to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.upload"),
                        format_bytes(data.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.download"),
                        format_bytes(data.download_total),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.memory"),
                        format_bytes(data.memory),
                        theme.warning,
                        theme,
                    )),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("connections.refresh_hint")),
                    )
                    .child(
                        Button::new("close-all-connections")
                            .icon(IconName::CircleX)
                            .label(zenclash_i18n::text("connections.actions.close_all"))
                            .danger()
                            .small()
                            .disabled(
                                total == 0 || self.mutating || !self.connections.closing.is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.connections.filter).small()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if query.is_empty() {
                            zenclash_i18n::text_with(
                                "connections.count.active",
                                &[("total", total.to_string())],
                            )
                        } else {
                            zenclash_i18n::text_with(
                                "common.count.visible_total",
                                &[
                                    ("visible", visible.to_string()),
                                    ("total", total.to_string()),
                                ],
                            )
                        },
                    )),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(filtered.is_empty(), |this| {
                        this.child(empty_state(
                            if total == 0 {
                                zenclash_i18n::text("connections.empty.active")
                            } else {
                                zenclash_i18n::text("connections.empty.filtered")
                            },
                            theme,
                        ))
                    })
                    .children(
                        filtered
                            .into_iter()
                            .enumerate()
                            .map(|(offset, connection)| {
                                let index = page.start + offset;
                                let id = connection.id.clone();
                                let closing = self.connections.closing.contains(&id);
                                let expanded =
                                    self.connections.expanded.as_deref() == Some(id.as_str());
                                let host = if connection.metadata.host.is_empty() {
                                    connection.metadata.destination_ip.clone()
                                } else {
                                    connection.metadata.host.clone()
                                };
                                let summary = connection_summary(connection);
                                let detail_id = id.clone();
                                v_flex()
                                    .id(("connection-row", index))
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        h_flex()
                                            .min_h(px(58.))
                                            .px_4()
                                            .gap_3()
                                            .items_center()
                                            .child(Icon::new(IconName::ExternalLink).size_4())
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(div().text_sm().child(format!(
                                                        "{}:{}",
                                                        host, connection.metadata.destination_port
                                                    )))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(summary),
                                                    ),
                                            )
                                            .child(
                                                v_flex()
                                                    .items_end()
                                                    .text_xs()
                                                    .child(format!(
                                                        "↑ {}",
                                                        format_bytes(connection.upload)
                                                    ))
                                                    .child(format!(
                                                        "↓ {}",
                                                        format_bytes(connection.download)
                                                    )),
                                            )
                                            .child(
                                                Button::new(("connection-details", index))
                                                    .icon(IconName::Eye)
                                                    .label(zenclash_i18n::text(if expanded {
                                                        "connections.actions.hide_details"
                                                    } else {
                                                        "connections.actions.show_details"
                                                    }))
                                                    .ghost()
                                                    .small()
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.toggle_connection_details(
                                                                detail_id.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new(("close-connection", index))
                                                    .icon(IconName::CircleX)
                                                    .label(zenclash_i18n::text(
                                                        "connections.actions.close",
                                                    ))
                                                    .ghost()
                                                    .small()
                                                    .disabled(self.mutating || closing)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.close_connection(id.clone(), cx);
                                                        },
                                                    )),
                                            ),
                                    )
                                    .when(expanded, |this| {
                                        this.child(
                                            v_flex()
                                                .px_12()
                                                .pb_4()
                                                .gap_2()
                                                .child(connection_detail(
                                                    zenclash_i18n::text(
                                                        "connections.details.source",
                                                    ),
                                                    format!(
                                                        "{}:{}",
                                                        connection.metadata.source_ip,
                                                        connection.metadata.source_port
                                                    ),
                                                    theme,
                                                ))
                                                .child(connection_detail(
                                                    zenclash_i18n::text(
                                                        "connections.details.destination",
                                                    ),
                                                    format!(
                                                        "{}:{}",
                                                        connection.metadata.destination_ip,
                                                        connection.metadata.destination_port
                                                    ),
                                                    theme,
                                                ))
                                                .child(connection_detail(
                                                    zenclash_i18n::text("connections.details.rule"),
                                                    format!(
                                                        "{} · {}",
                                                        connection.rule, connection.rule_payload
                                                    ),
                                                    theme,
                                                ))
                                                .child(connection_detail(
                                                    zenclash_i18n::text(
                                                        "connections.details.route",
                                                    ),
                                                    connection.chains.join(" → "),
                                                    theme,
                                                )),
                                        )
                                    })
                            }),
                    ),
            )
            .when(page.count > 1, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(pagination_summary(page, visible)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("previous-connections-page")
                                        .icon(IconName::ChevronLeft)
                                        .label(zenclash_i18n::text("common.actions.previous_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index == 0)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_connections_page(previous_page, cx);
                                        })),
                                )
                                .child(
                                    Button::new("next-connections-page")
                                        .icon(IconName::ChevronRight)
                                        .label(zenclash_i18n::text("common.actions.next_page"))
                                        .small()
                                        .outline()
                                        .disabled(page.index + 1 >= page.count)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_connections_page(next_page, cx);
                                        })),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}

fn connection_detail(label: String, value: String, theme: &gpui_component::Theme) -> gpui::Div {
    h_flex()
        .gap_3()
        .text_xs()
        .child(div().w_24().text_color(theme.muted_foreground).child(label))
        .child(div().min_w_0().child(if value.is_empty() {
            "—".into()
        } else {
            value
        }))
}

fn normalize_connection_query(query: &str) -> String {
    query.trim().to_owned()
}

fn connection_matches(connection: &zenclash_core::Connection, query: &str) -> bool {
    query.is_empty()
        || contains_ascii_case_insensitive(&connection.metadata.host, query)
        || contains_ascii_case_insensitive(&connection.metadata.destination_ip, query)
        || contains_ascii_case_insensitive(&connection.metadata.source_ip, query)
        || contains_ascii_case_insensitive(&connection.metadata.process, query)
        || contains_ascii_case_insensitive(&connection.rule, query)
        || contains_ascii_case_insensitive(&connection.rule_payload, query)
        || connection
            .chains
            .iter()
            .any(|chain| contains_ascii_case_insensitive(chain, query))
}

fn connection_summary(connection: &zenclash_core::Connection) -> String {
    let mut parts = Vec::with_capacity(4);
    if !connection.metadata.network.is_empty() {
        parts.push(connection.metadata.network.clone());
    }
    if !connection.metadata.process.is_empty() {
        parts.push(connection.metadata.process.clone());
    }
    if !connection.rule.is_empty() {
        parts.push(connection.rule.clone());
    }
    if !connection.chains.is_empty() {
        parts.push(connection.chains.join(" → "));
    }
    if parts.is_empty() {
        zenclash_i18n::text("connections.empty.details")
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_connection_identity_and_route_fields() {
        let connection = zenclash_core::Connection {
            metadata: zenclash_core::ConnectionMetadata {
                host: "Example.COM".into(),
                destination_ip: "203.0.113.8".into(),
                process: "Browser".into(),
                ..Default::default()
            },
            rule: "DomainSuffix".into(),
            chains: vec!["Hong Kong".into()],
            ..Default::default()
        };

        for query in ["example", "203.0.113", "browser", "domainsuffix", "hong"] {
            assert!(connection_matches(
                &connection,
                &normalize_connection_query(query)
            ));
        }
        assert!(!connection_matches(
            &connection,
            &normalize_connection_query("direct")
        ));
        assert_eq!(
            connection_summary(&connection),
            "Browser · DomainSuffix · Hong Kong"
        );
    }
}
