use std::collections::HashSet;

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{ButtonGroup, ButtonVariant};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

use super::{
    AppContext, Button, ButtonVariants, Context, Disableable, Entity, FluentBuilder, IconName,
    Input, InputEvent, InputState, IntoElement, ParentElement, Sizable, Styled, Subscription,
    Window, contains_ascii_case_insensitive, div, empty_state, format_bytes, h_flex, list_page,
    pagination_summary, v_flex,
};

mod dashboard;
mod page_integration;
mod projection;
pub(in crate::pages::runtime) mod snapshot_read;
mod timeline;

const CONNECTIONS_PER_PAGE: usize = 50;

pub(super) struct ConnectionsView {
    runtime: tokio::runtime::Handle,
    live_snapshot: Option<std::sync::Arc<zenclash_core::ConnectionsSnapshot>>,
    runtime_context: Option<ConnectionSnapshotContext>,
    close_allowed: bool,
    focus_handle: gpui_kit::FocusHandle,
    _filter_subscription: Subscription,
    filter_input: Entity<InputState>,
    closing_connections: HashSet<String>,
    inspected_connection: Option<String>,
    page_index: usize,
    scroll: gpui_kit::ScrollHandle,
    transport: ConnectionTransport,
    sort: ConnectionSort,
    query: String,
    projection: Option<projection::ConnectionProjection>,
    worker: projection::ProjectionWorker,
    projecting: bool,
    paused_snapshot: Option<std::sync::Arc<zenclash_core::ConnectionsSnapshot>>,
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

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ConnectionSnapshotContext {
    configuration_generation: u64,
    profile_path: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub(super) enum ConnectionEvent {
    CloseRequested {
        id: Option<String>,
        runtime: ConnectionSnapshotContext,
    },
    PresentationFailed(String),
}

impl gpui_kit::EventEmitter<ConnectionEvent> for ConnectionsView {}

impl ConnectionsView {
    pub(super) fn release_presentation(&mut self) {
        self.worker.cancel();
        self.live_snapshot = None;
        self.runtime_context = None;
        self.projection = None;
        self.projecting = false;
        self.paused_snapshot = None;
        self.history = ConnectionMetricHistory::default();
    }

    pub(super) fn new(
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text(
                "runtime.placeholders.connection_filter",
            ))
        });
        let subscription = cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if !replace_connection_query(&mut this.query, &input.read(cx).value()) {
                    return;
                }
                this.page_index = 0;
                this.update_projection(cx);
                cx.notify();
            }
        });
        Self {
            runtime,
            live_snapshot: None,
            runtime_context: None,
            close_allowed: false,
            focus_handle: cx.focus_handle(),
            _filter_subscription: subscription,
            filter_input: filter,
            closing_connections: HashSet::new(),
            inspected_connection: None,
            page_index: 0,
            scroll: gpui_kit::ScrollHandle::default(),
            transport: ConnectionTransport::All,
            sort: ConnectionSort::Default,
            query: String::new(),
            projection: None,
            worker: projection::ProjectionWorker::default(),
            projecting: false,
            paused_snapshot: None,
            history: ConnectionMetricHistory::default(),
            show_closed: false,
            timeline: timeline::ConnectionTimeline::default(),
        }
    }
}

impl ConnectionsView {
    pub(super) fn present(
        &mut self,
        live_snapshot: std::sync::Arc<zenclash_core::ConnectionsSnapshot>,
        runtime: ConnectionSnapshotContext,
        cx: &mut Context<Self>,
    ) {
        if self
            .runtime_context
            .as_ref()
            .is_some_and(|previous| previous != &runtime)
        {
            self.release_presentation();
        }
        self.live_snapshot = Some(live_snapshot);
        self.runtime_context = Some(runtime);
        self.update_projection(cx);
    }

    pub(super) fn is_idle(&self) -> bool {
        self.closing_connections.is_empty() && !self.projecting
    }

    pub(super) fn refresh_localized_placeholder(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_input.update(cx, |input, cx| {
            input.set_placeholder(
                zenclash_i18n::text("runtime.placeholders.connection_filter"),
                window,
                cx,
            )
        });
    }

    pub(super) fn set_close_allowed(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if self.close_allowed != allowed {
            self.close_allowed = allowed;
            cx.notify();
        }
    }

    pub(super) fn has_pending_closes(&self) -> bool {
        !self.closing_connections.is_empty()
    }

    pub(super) fn begin_close(&mut self, id: String, cx: &mut Context<Self>) -> bool {
        let admitted = self.close_allowed && self.closing_connections.insert(id);
        if admitted {
            cx.notify();
        }
        admitted
    }

    pub(super) fn finish_connection_close(&mut self, id: &str, cx: &mut Context<Self>) {
        self.closing_connections.remove(id);
        cx.notify();
    }

    pub(super) fn resume_live_display(&mut self, cx: &mut Context<Self>) {
        self.paused_snapshot = None;
        cx.notify();
    }

    fn update_projection(&mut self, cx: &mut Context<Self>) {
        let mut data = match self
            .paused_snapshot
            .as_ref()
            .or(self.live_snapshot.as_ref())
        {
            Some(snapshot) => snapshot.clone(),
            _ => {
                self.release_presentation();
                return;
            }
        };
        let Some(runtime_context) = self.runtime_context.clone() else {
            return;
        };
        if self.paused_snapshot.is_none() {
            self.timeline.observe(
                runtime_context.configuration_generation,
                data.clone(),
                std::time::Instant::now(),
            );
        }
        if self.show_closed {
            data = self.timeline.closed();
        }

        if self.paused_snapshot.is_some()
            && self.projection.as_ref().is_some_and(|projection| {
                std::sync::Arc::ptr_eq(&projection.snapshot, &data)
                    && projection.query == self.query
                    && projection.transport == self.transport
                    && projection.sort == self.sort
            })
        {
            return;
        }
        let (generation, task) = self.worker.start(
            &self.runtime,
            data,
            self.query.clone(),
            self.transport,
            self.sort,
        );
        self.projecting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.worker.is_current(generation) {
                    return;
                }
                this.projecting = false;
                match result {
                    Ok(Some(projection)) => {
                        this.history.observe(
                            runtime_context.configuration_generation,
                            &projection.snapshot,
                        );
                        this.projection = Some(projection);
                    }
                    Ok(None) => {}
                    Err(error) => cx.emit(ConnectionEvent::PresentationFailed(
                        zenclash_i18n::text_with(
                            "connections.errors.filter_task",
                            &[("error", error)],
                        ),
                    )),
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
            .icon(if self.paused_snapshot.is_some() {
                gpui_kit::assets::IconName::Play
            } else {
                gpui_kit::assets::IconName::Pause
            })
            .label(zenclash_i18n::text(if self.paused_snapshot.is_some() {
                "common.actions.resume_display"
            } else {
                "common.actions.pause_display"
            }))
            .tooltip(zenclash_i18n::text("connections.display_pause_description"))
            .small()
            .h_10()
            .outline()
            .disabled(self.projection.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                this.paused_snapshot = if this.paused_snapshot.is_some() {
                    None
                } else {
                    this.projection
                        .as_ref()
                        .map(|projection| projection.snapshot.clone())
                };
                this.worker.cancel();
                this.projecting = false;
                this.update_projection(cx);
                cx.notify();
            }))
    }

    fn render_connection_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let transport = self.transport;
        let sort = self.sort;
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
                                this.transport = value;
                                this.page_index = 0;
                                this.update_projection(cx);
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
                                        let _ = owner.update(cx, |view, cx| {
                                            view.sort = value;
                                            view.page_index = 0;
                                            view.update_projection(cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn inspect_for_design_validation(
        &mut self,
        id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.inspected_connection = id;
        cx.notify();
    }

    fn toggle_connection_details(&mut self, id: String, cx: &mut Context<Self>) {
        self.inspected_connection = if self.inspected_connection.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    fn set_connections_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.page_index = page;
        self.scroll
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
        let Some(runtime) = self.runtime_context.clone() else {
            return;
        };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            let id = id.clone();
            let runtime = runtime.clone();
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
                    let _ = owner.update(cx, |view, cx| {
                        if view.runtime_context.as_ref() != Some(&runtime) || !view.close_allowed {
                            return;
                        }
                        cx.emit(ConnectionEvent::CloseRequested {
                            id: id.clone(),
                            runtime: runtime.clone(),
                        });
                    });
                    true
                })
        });
    }
}

impl gpui_kit::Render for ConnectionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme;
        let theme = cx.theme().clone();
        self.connection_dashboard(
            window.viewport_size().width < window.rem_size() * 68.,
            &theme,
            cx,
        )
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

#[cfg(test)]
mod ui_tests;
