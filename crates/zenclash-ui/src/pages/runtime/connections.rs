use std::collections::HashSet;

use gpui_component::menu::{DropdownMenu, PopupMenuItem};

use super::{
    AppContext, Button, ButtonVariants, Context, Disableable, Entity, FluentBuilder, Icon,
    IconName, Input, InputEvent, InputState, InteractiveElement, IntoElement, Page, ParentElement,
    RuntimeData, RuntimePage, Sizable, Styled, Subscription, Window,
    contains_ascii_case_insensitive, div, empty_state, format_bytes, h_flex, list_page,
    message_banner, metric, pagination_summary, px, v_flex,
};

mod projection;

const CONNECTIONS_PER_PAGE: usize = 100;

pub(super) struct ConnectionsUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) closing: HashSet<String>,
    pub(super) expanded: Option<String>,
    pub(super) page: usize,
    transport: ConnectionTransport,
    sort: ConnectionSort,
    query: String,
    projection: Option<projection::ConnectionProjection>,
    worker: projection::ProjectionWorker,
    pub(super) projecting: bool,
}

impl ConnectionsUiState {
    pub(super) fn release_presentation(&mut self) {
        self.worker.cancel();
        self.projection = None;
        self.projecting = false;
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text(
                "runtime.placeholders.connection_filter",
            ))
        });
        let subscription = cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if !replace_connection_query(&mut this.connections.query, &input.read(cx).value()) {
                    return;
                }
                this.connections.page = 0;
                this.update_connection_presentation(cx);
                cx.notify();
            }
        });
        (
            Self {
                filter,
                closing: HashSet::new(),
                expanded: None,
                page: 0,
                transport: ConnectionTransport::All,
                sort: ConnectionSort::Default,
                query: String::new(),
                projection: None,
                worker: projection::ProjectionWorker::default(),
                projecting: false,
            },
            subscription,
        )
    }
}

impl RuntimePage {
    pub(super) fn update_connection_presentation(&mut self, cx: &mut Context<Self>) {
        let RuntimeData::Connections(data) = &self.data else {
            self.connections.release_presentation();
            return;
        };
        if self.page != Page::Connections {
            return;
        }
        let (generation, task) = self.connections.worker.start(
            &self.runtime,
            data.clone(),
            self.connections.query.clone(),
            self.connections.transport,
            self.connections.sort,
        );
        self.connections.projecting = true;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.connections.worker.is_current(generation) {
                    return;
                }
                this.connections.projecting = false;
                match result {
                    Ok(Some(projection)) => this.connections.projection = Some(projection),
                    Ok(None) => {}
                    Err(error) => {
                        this.error = Some(zenclash_i18n::text_with(
                            "connections.errors.filter_task",
                            &[("error", error.to_string())],
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_connection_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let transport = self.connections.transport;
        let sort = self.connections.sort;
        let transport_owner = cx.entity().downgrade();
        let sort_owner = cx.entity().downgrade();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("connection-transport")
                    .label(zenclash_i18n::text(transport.label()))
                    .small()
                    .outline()
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for value in ConnectionTransport::ALL {
                            let owner = transport_owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(zenclash_i18n::text(value.label()))
                                    .checked(value == transport)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |page, cx| {
                                            page.connections.transport = value;
                                            page.connections.page = 0;
                                            page.update_connection_presentation(cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("connection-sort")
                    .label(zenclash_i18n::text(sort.label()))
                    .small()
                    .outline()
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for value in ConnectionSort::ALL {
                            let owner = sort_owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(zenclash_i18n::text(value.label()))
                                    .checked(value == sort)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |page, cx| {
                                            page.connections.sort = value;
                                            page.connections.page = 0;
                                            page.update_connection_presentation(cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

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
                .map(|data| RuntimeData::Connections(std::sync::Arc::new(data)))
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
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data, cx) {
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
        if self.core_busy() || !self.connections.closing.insert(id.clone()) {
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
        let Some(projection) = &self.connections.projection else {
            return v_flex()
                .child(Input::new(&self.connections.filter).small())
                .child(empty_state(
                    zenclash_i18n::text(if self.connections.projecting {
                        "connections.filtering"
                    } else {
                        "connections.empty.active"
                    }),
                    theme,
                ))
                .into_any_element();
        };
        let data = &projection.snapshot;
        let total = data.connections.len();
        let query = &projection.query;
        let filtered = &projection.order;
        let visible = filtered.len();
        let page = list_page(visible, self.connections.page, CONNECTIONS_PER_PAGE);
        let filtered = &filtered[page.start..page.end];
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
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text(if self.connections.projecting {
                            "connections.filtering"
                        } else {
                            "connections.refresh_hint"
                        }),
                    ))
                    .child(
                        Button::new("close-all-connections")
                            .icon(IconName::CircleX)
                            .label(zenclash_i18n::text("connections.actions.close_all"))
                            .danger()
                            .small()
                            .disabled(
                                total == 0
                                    || self.core_busy()
                                    || !self.connections.closing.is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx))),
                    ),
            )
            .child(self.render_connection_options(cx))
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
                    .children(filtered.iter().map(|&index| {
                        let connection = &data.connections[index];
                        let id = connection.id.clone();
                        let closing = self.connections.closing.contains(&id);
                        let expanded = self.connections.expanded.as_deref() == Some(id.as_str());
                        let host = if connection.metadata.host.is_empty() {
                            connection.metadata.destination_ip.clone()
                        } else {
                            connection.metadata.host.clone()
                        };
                        let summary = connection_summary(connection);
                        let detail_id = id.clone();
                        v_flex()
                            .id(connection_element_id("connection-row", &id))
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
                                            .child(format!("↑ {}", format_bytes(connection.upload)))
                                            .child(format!(
                                                "↓ {}",
                                                format_bytes(connection.download)
                                            )),
                                    )
                                    .child(
                                        Button::new(connection_element_id(
                                            "connection-details",
                                            &id,
                                        ))
                                        .icon(IconName::Eye)
                                        .label(zenclash_i18n::text(if expanded {
                                            "connections.actions.hide_details"
                                        } else {
                                            "connections.actions.show_details"
                                        }))
                                        .ghost()
                                        .small()
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.toggle_connection_details(
                                                    detail_id.clone(),
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(connection_element_id("close-connection", &id))
                                            .icon(IconName::CircleX)
                                            .label(zenclash_i18n::text("connections.actions.close"))
                                            .ghost()
                                            .small()
                                            .disabled(self.core_busy() || closing)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.close_connection(id.clone(), cx);
                                            })),
                                    ),
                            )
                            .when(expanded, |this| {
                                this.child(
                                    v_flex()
                                        .px_12()
                                        .pb_4()
                                        .gap_2()
                                        .child(connection_detail(
                                            zenclash_i18n::text("connections.details.source"),
                                            format!(
                                                "{}:{}",
                                                connection.metadata.source_ip,
                                                connection.metadata.source_port
                                            ),
                                            theme,
                                        ))
                                        .child(connection_detail(
                                            zenclash_i18n::text("connections.details.destination"),
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
                                            zenclash_i18n::text("connections.details.route"),
                                            connection.chains.join(" → "),
                                            theme,
                                        )),
                                )
                            })
                    })),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionTransport {
    All,
    Tcp,
    Udp,
}

impl ConnectionTransport {
    const ALL: [Self; 3] = [Self::All, Self::Tcp, Self::Udp];
    const fn label(self) -> &'static str {
        match self {
            Self::All => "connections.transport.all",
            Self::Tcp => "connections.transport.tcp",
            Self::Udp => "connections.transport.udp",
        }
    }
    fn matches(self, network: &str) -> bool {
        match self {
            Self::All => true,
            Self::Tcp => network.eq_ignore_ascii_case("tcp"),
            Self::Udp => network.eq_ignore_ascii_case("udp"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionSort {
    Default,
    Newest,
    Oldest,
    Upload,
    Download,
    Total,
}

impl ConnectionSort {
    const ALL: [Self; 6] = [
        Self::Default,
        Self::Newest,
        Self::Oldest,
        Self::Upload,
        Self::Download,
        Self::Total,
    ];
    const fn label(self) -> &'static str {
        match self {
            Self::Default => "connections.sort.default",
            Self::Newest => "connections.sort.newest",
            Self::Oldest => "connections.sort.oldest",
            Self::Upload => "connections.sort.upload",
            Self::Download => "connections.sort.download",
            Self::Total => "connections.sort.total",
        }
    }
}

fn present_connections(
    connections: &[zenclash_core::Connection],
    query: &str,
    transport: ConnectionTransport,
    sort: ConnectionSort,
) -> Vec<usize> {
    let mut result = (0..connections.len())
        .filter(|&index| {
            let connection = &connections[index];
            transport.matches(&connection.metadata.network) && connection_matches(connection, query)
        })
        .collect::<Vec<_>>();
    match sort {
        ConnectionSort::Default => {}
        ConnectionSort::Newest | ConnectionSort::Oldest => result.sort_by_cached_key(|&index| {
            let connection = &connections[index];
            let timestamp = chrono::DateTime::parse_from_rfc3339(&connection.start)
                .ok()
                .map(|value| value.timestamp_micros());
            (
                timestamp.is_none(),
                timestamp.map(|value| {
                    if sort == ConnectionSort::Newest {
                        -i128::from(value)
                    } else {
                        i128::from(value)
                    }
                }),
                connection.id.clone(),
            )
        }),
        _ => result.sort_by_cached_key(|&index| {
            let connection = &connections[index];
            let bytes = match sort {
                ConnectionSort::Upload => u128::from(connection.upload),
                ConnectionSort::Download => u128::from(connection.download),
                _ => u128::from(connection.upload) + u128::from(connection.download),
            };
            (std::cmp::Reverse(bytes), connection.id.clone())
        }),
    }
    result
}

fn connection_element_id(action: &'static str, id: &str) -> gpui::ElementId {
    (gpui::ElementId::from(action), id.to_owned()).into()
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

fn replace_connection_query(current: &mut String, incoming: &str) -> bool {
    if current.eq_ignore_ascii_case(incoming.trim()) {
        return false;
    }
    *current = normalize_connection_query(incoming);
    true
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
    fn protocol_filter_and_traffic_sort_apply_before_pagination() {
        let connections = vec![
            zenclash_core::Connection {
                id: "udp".into(),
                download: 100,
                metadata: zenclash_core::ConnectionMetadata {
                    network: "UDP".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            zenclash_core::Connection {
                id: "small".into(),
                download: 1,
                metadata: zenclash_core::ConnectionMetadata {
                    network: "tcp".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            zenclash_core::Connection {
                id: "large".into(),
                download: 50,
                metadata: zenclash_core::ConnectionMetadata {
                    network: "TCP".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        let result = present_connections(
            &connections,
            "",
            ConnectionTransport::Tcp,
            ConnectionSort::Download,
        );
        assert_eq!(
            result
                .iter()
                .map(|&index| connections[index].id.as_str())
                .collect::<Vec<_>>(),
            ["large", "small"]
        );
    }

    #[test]
    fn newest_sort_compares_instants_and_puts_invalid_dates_last() {
        let connections = vec![
            zenclash_core::Connection {
                id: "invalid".into(),
                ..Default::default()
            },
            zenclash_core::Connection {
                id: "older".into(),
                start: "2026-01-01T10:00:00+08:00".into(),
                ..Default::default()
            },
            zenclash_core::Connection {
                id: "newer".into(),
                start: "2026-01-01T03:00:00Z".into(),
                ..Default::default()
            },
        ];
        let result = present_connections(
            &connections,
            "",
            ConnectionTransport::All,
            ConnectionSort::Newest,
        );
        assert_eq!(
            result
                .iter()
                .map(|&index| connections[index].id.as_str())
                .collect::<Vec<_>>(),
            ["newer", "older", "invalid"]
        );
    }

    #[test]
    fn connection_controls_keep_identity_after_reordering() {
        let id = connection_element_id("close-connection", "connection-a");
        assert_eq!(
            id,
            connection_element_id("close-connection", "connection-a")
        );
        assert_ne!(
            id,
            connection_element_id("close-connection", "connection-b")
        );
        assert_ne!(
            id,
            connection_element_id("connection-details", "connection-a")
        );
    }

    #[test]
    fn equivalent_edits_keep_the_running_query_and_real_edits_replace_it() {
        let mut current = "example.com".to_owned();
        for incoming in ["example.com", "  example.com  ", "EXAMPLE.COM"] {
            assert!(!replace_connection_query(&mut current, incoming));
            assert_eq!(current, "example.com");
        }
        assert!(replace_connection_query(&mut current, " example.org "));
        assert_eq!(current, "example.org");
        assert!(replace_connection_query(&mut current, " "));
        assert!(current.is_empty());
        assert!(!replace_connection_query(&mut current, "\t"));
        assert!(replace_connection_query(&mut current, "节点甲"));
        assert!(replace_connection_query(&mut current, "节点乙"));
    }

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
