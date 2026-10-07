use std::collections::HashSet;

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{ButtonGroup, ButtonVariant};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

use super::{
    AppContext, Button, ButtonVariants, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, IntoElement, Page, ParentElement, RuntimeData, RuntimePage,
    Sizable, Styled, Subscription, Window, contains_ascii_case_insensitive, div, empty_state,
    format_bytes, h_flex, list_page, pagination_summary, v_flex,
};

mod dashboard;
mod projection;
mod timeline;

const CONNECTIONS_PER_PAGE: usize = 50;

pub(super) struct ConnectionsUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) closing: HashSet<String>,
    pub(super) expanded: Option<String>,
    pub(super) page: usize,
    scroll: gpui_kit::ScrollHandle,
    transport: ConnectionTransport,
    sort: ConnectionSort,
    query: String,
    projection: Option<projection::ConnectionProjection>,
    worker: projection::ProjectionWorker,
    pub(super) projecting: bool,
    frozen: Option<std::sync::Arc<zenclash_core::ConnectionsSnapshot>>,
    history: ConnectionMetricHistory,
    show_closed: bool,
    timeline: timeline::ConnectionTimeline,
}

#[derive(Default)]
struct ConnectionMetricHistory {
    generation: Option<u64>,
    last_snapshot: Option<std::sync::Weak<zenclash_core::ConnectionsSnapshot>>,
    samples: std::collections::VecDeque<[u64; 4]>,
}

impl ConnectionMetricHistory {
    fn observe(
        &mut self,
        generation: u64,
        snapshot: &std::sync::Arc<zenclash_core::ConnectionsSnapshot>,
    ) {
        if self.generation != Some(generation) {
            *self = Self::default();
            self.generation = Some(generation);
        }
        if self
            .last_snapshot
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|last| std::sync::Arc::ptr_eq(&last, snapshot))
        {
            return;
        }
        let sample = [
            snapshot.connections.len() as u64,
            snapshot.upload_total,
            snapshot.download_total,
            snapshot.memory,
        ];
        if self
            .samples
            .back()
            .is_some_and(|last| sample[1] < last[1] || sample[2] < last[2])
        {
            self.samples.clear();
        }
        self.last_snapshot = Some(std::sync::Arc::downgrade(snapshot));
        self.samples.push_back(sample);
        if self.samples.len() > 60 {
            self.samples.pop_front();
        }
    }

    fn points(&self, metric: usize) -> Vec<(gpui_kit::SharedString, f64)> {
        self.samples
            .iter()
            .enumerate()
            .map(|(index, sample)| (index.to_string().into(), sample[metric] as f64))
            .collect()
    }
}

impl ConnectionsUiState {
    pub(super) fn release_presentation(&mut self) {
        self.worker.cancel();
        self.projection = None;
        self.projecting = false;
        self.frozen = None;
        self.history = ConnectionMetricHistory::default();
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
                scroll: gpui_kit::ScrollHandle::default(),
                transport: ConnectionTransport::All,
                sort: ConnectionSort::Default,
                query: String::new(),
                projection: None,
                worker: projection::ProjectionWorker::default(),
                projecting: false,
                frozen: None,
                history: ConnectionMetricHistory::default(),
                show_closed: false,
                timeline: timeline::ConnectionTimeline::default(),
            },
            subscription,
        )
    }
}

impl RuntimePage {
    pub(super) fn update_connection_presentation(&mut self, cx: &mut Context<Self>) {
        let mut data = match (&self.connections.frozen, &self.data) {
            (Some(snapshot), _) | (None, RuntimeData::Connections(snapshot)) => snapshot.clone(),
            _ => {
                self.connections.release_presentation();
                return;
            }
        };
        if self.page != Page::Connections {
            return;
        }
        if self.connections.frozen.is_none() {
            self.connections.timeline.observe(
                self.core_session.generation(),
                data.clone(),
                std::time::Instant::now(),
            );
        }
        if self.connections.show_closed {
            data = self.connections.timeline.closed();
        }

        if self.connections.frozen.is_some()
            && self
                .connections
                .projection
                .as_ref()
                .is_some_and(|projection| {
                    std::sync::Arc::ptr_eq(&projection.snapshot, &data)
                        && projection.query == self.connections.query
                        && projection.transport == self.connections.transport
                        && projection.sort == self.connections.sort
                })
        {
            return;
        }
        let (generation, task) = self.connections.worker.start(
            &self.runtime,
            data,
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
                    Ok(Some(projection)) => {
                        this.connections
                            .history
                            .observe(this.core_session.generation(), &projection.snapshot);
                        this.connections.projection = Some(projection);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.error = Some(zenclash_i18n::text_with(
                            "connections.errors.filter_task",
                            &[("error", error)],
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::pages::runtime) fn render_connection_pause(
        &self,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new("pause-connections-display")
            .icon(if self.connections.frozen.is_some() {
                gpui_kit::assets::IconName::Play
            } else {
                gpui_kit::assets::IconName::Pause
            })
            .label(zenclash_i18n::text(if self.connections.frozen.is_some() {
                "common.actions.resume_display"
            } else {
                "common.actions.pause_display"
            }))
            .tooltip(zenclash_i18n::text("connections.display_pause_description"))
            .small()
            .h_10()
            .outline()
            .disabled(self.connections.projection.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                this.connections.frozen = if this.connections.frozen.is_some() {
                    None
                } else {
                    this.connections
                        .projection
                        .as_ref()
                        .map(|projection| projection.snapshot.clone())
                };
                this.connections.worker.cancel();
                this.connections.projecting = false;
                this.update_connection_presentation(cx);
                cx.notify();
            }))
    }

    fn render_connection_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let transport = self.connections.transport;
        let sort = self.connections.sort;
        let sort_owner = cx.entity().downgrade();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                ButtonGroup::new("connection-transport-group")
                    .outline()
                    .children(ConnectionTransport::ALL.into_iter().map(|value| {
                        use gpui_kit::component::Selectable as _;
                        Button::new(("connection-transport", value as usize))
                            .label(zenclash_i18n::text(value.label()))
                            .small()
                            .h_10()
                            .min_w(gpui_kit::rems(4.))
                            .outline()
                            .selected(value == transport)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.connections.transport = value;
                                this.connections.page = 0;
                                this.update_connection_presentation(cx);
                                cx.notify();
                            }))
                    })),
            )
            .child(
                Button::new("connection-sort")
                    .label(zenclash_i18n::text(sort.label()))
                    .small()
                    .h_10()
                    .min_w(gpui_kit::rems(8.))
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
        self.connections
            .scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        cx.notify();
    }

    fn confirm_connection_close(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        let owner = cx.entity().downgrade();
        let generation = self.core_session.generation();
        let profile = self.profile_path.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            let id = id.clone();
            let profile = profile.clone();
            dialog
                .confirm()
                .title(zenclash_i18n::text(if id.is_some() {
                    "connections.ui.close_title"
                } else {
                    "connections.ui.close_all_title"
                }))
                .description(zenclash_i18n::text("connections.ui.close_description"))
                .ok_text(zenclash_i18n::text("connections.ui.close_confirm"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = owner.update(cx, |page, cx| {
                        if page.page != Page::Connections
                            || page.core_session.generation() != generation
                            || page.profile_path != profile
                        {
                            return;
                        }
                        if let Some(id) = &id {
                            page.close_connection(id.clone(), cx);
                        } else {
                            page.close_all_connections(cx);
                        }
                    });
                    true
                })
        });
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
                        if this.is_page_task_current(token) {
                            this.connections.frozen = None;
                        }
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
                    Ok(()) if this.is_page_task_current(token) => {
                        this.connections.frozen = None;
                        this.refresh(cx);
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_connections(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        self.connection_dashboard(compact, theme, cx)
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

fn connection_element_id(action: &'static str, id: &str) -> gpui_kit::ElementId {
    (gpui_kit::ElementId::from(action), id.to_owned()).into()
}

fn connection_detail(
    label: String,
    value: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(if value.is_empty() {
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

#[cfg(test)]
fn connection_summary(connection: &zenclash_core::Connection) -> String {
    let mut parts = Vec::with_capacity(4);
    for value in [
        &connection.metadata.network,
        &connection.metadata.process,
        &connection.rule,
    ] {
        if !value.is_empty() {
            parts.push(value.clone());
        }
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

#[cfg(test)]
mod history_tests {
    use super::ConnectionMetricHistory;
    use std::sync::Arc;
    use zenclash_core::{Connection, ConnectionsSnapshot};

    fn snapshot(upload: u64) -> Arc<ConnectionsSnapshot> {
        Arc::new(ConnectionsSnapshot {
            connections: vec![Connection::default()],
            upload_total: upload,
            download_total: upload * 2,
            memory: 1024,
        })
    }

    #[test]
    fn metric_history_deduplicates_filtered_snapshots_and_resets_on_core_or_counter_change() {
        let mut history = ConnectionMetricHistory::default();
        let first = snapshot(100);
        history.observe(1, &first);
        history.observe(1, &first);
        assert_eq!(history.samples.len(), 1);
        history.observe(1, &snapshot(200));
        assert_eq!(
            history
                .points(1)
                .iter()
                .map(|point| point.1)
                .collect::<Vec<_>>(),
            [100., 200.]
        );
        history.observe(2, &snapshot(300));
        assert_eq!(history.samples.len(), 1);
        history.observe(2, &snapshot(10));
        assert_eq!(history.samples.len(), 1);
        assert_eq!(history.samples[0], [1, 10, 20, 1024]);
    }

    #[test]
    fn metric_history_retains_only_observed_recent_samples_without_retaining_connection_snapshots()
    {
        let mut history = ConnectionMetricHistory::default();
        let first = snapshot(1);
        let old = Arc::downgrade(&first);
        history.observe(1, &first);
        drop(first);
        for upload in 2..=120 {
            history.observe(1, &snapshot(upload));
        }
        assert!(old.upgrade().is_none());
        assert_eq!(history.samples.len(), 60);
        assert_eq!(history.samples.front().unwrap()[1], 61);
        assert_eq!(history.samples.back().unwrap()[1], 120);
    }
}
