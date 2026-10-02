use super::*;
use gpui_kit::component::chart::PieChart;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use zenclash_core::{Connection, ConnectionsSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Proxy,
    Direct,
    Reject,
    Unknown,
}
impl Route {
    fn index(self) -> usize {
        match self {
            Self::Proxy => 0,
            Self::Direct => 1,
            Self::Reject => 2,
            Self::Unknown => 3,
        }
    }
    fn label(self) -> String {
        zenclash_i18n::text(match self {
            Self::Proxy => "home.flow.proxy",
            Self::Direct => "home.flow.direct",
            Self::Reject => "home.flow.reject",
            Self::Unknown => "common.status.unknown",
        })
    }
}
fn route(connection: &Connection) -> Route {
    match connection.chains.first().map(|chain| chain.trim()) {
        None | Some("") => Route::Unknown,
        Some(chain) if chain.eq_ignore_ascii_case("DIRECT") => Route::Direct,
        Some(chain)
            if chain.eq_ignore_ascii_case("REJECT")
                || chain.eq_ignore_ascii_case("REJECT-DROP") =>
        {
            Route::Reject
        }
        Some(_) => Route::Proxy,
    }
}
struct RecentConnection {
    host: String,
    process: String,
    bytes: u64,
    route: Route,
}
#[derive(Default)]
struct FlowSummary {
    routes: [u64; 4],
    recent: Vec<RecentConnection>,
}

fn aggregate(
    snapshot: ConnectionsSnapshot,
    epoch: &AtomicU64,
    expected: u64,
) -> Option<FlowSummary> {
    let mut routes = [0u64; 4];
    let mut recent = Vec::<(chrono::DateTime<chrono::FixedOffset>, usize)>::new();
    for (index, connection) in snapshot.connections.iter().enumerate() {
        if index.is_multiple_of(256) && epoch.load(Ordering::Acquire) != expected {
            return None;
        }
        routes[route(connection).index()] += 1;
        if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&connection.start) {
            let position = recent
                .iter()
                .position(|(time, _)| start > *time)
                .unwrap_or(recent.len());
            if position < 2 {
                recent.insert(position, (start, index));
                recent.truncate(2);
            }
        }
    }
    let recent = recent
        .into_iter()
        .map(|(_, index)| {
            let connection = &snapshot.connections[index];
            RecentConnection {
                host: if connection.metadata.host.is_empty() {
                    connection.metadata.destination_ip.clone()
                } else {
                    connection.metadata.host.clone()
                },
                process: connection.metadata.process.clone(),
                bytes: connection.upload.saturating_add(connection.download),
                route: route(connection),
            }
        })
        .collect();
    (epoch.load(Ordering::Acquire) == expected).then_some(FlowSummary { routes, recent })
}

#[derive(Default)]
pub(super) struct HomeFlowState {
    epoch: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::super::loader::PageReadTask,
    in_flight: bool,
    generation: u64,
    requested_at: Option<Instant>,
    summary: Option<FlowSummary>,
    error: Option<String>,
}
impl HomeFlowState {
    fn finish(
        &mut self,
        expected: u64,
        generation: u64,
        result: Result<Option<FlowSummary>, String>,
    ) -> bool {
        if self.epoch.load(Ordering::Acquire) != expected || self.generation != generation {
            return false;
        }
        self.in_flight = false;
        match result {
            Ok(Some(summary)) => {
                self.summary = Some(summary);
                self.error = None;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
        true
    }
    fn release(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
        self.in_flight = false;
        self.requested_at = None;
        self.summary = None;
        self.error = None;
    }
}
impl Drop for HomeFlowState {
    fn drop(&mut self) {
        self.release();
    }
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn release_home_presentation(&mut self) {
        self.home.flow.release();
        self.home.history.release();
        self.home.chart = traffic::HomeChartState::default();
        self.home.projection = None;
    }
    pub(super) fn update_home_flow(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Home {
            self.home.flow.release();
            return;
        }
        let generation = self.core_session.generation();
        if self.home.flow.generation != generation {
            self.home.flow.release();
            self.home.flow.generation = generation;
        }
        let state = &mut self.home.flow;
        if state.in_flight
            || state
                .requested_at
                .is_some_and(|time| time.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        state.in_flight = true;
        state.requested_at = Some(Instant::now());
        let expected = state.epoch.load(Ordering::Acquire);
        let epoch = state.epoch.clone();
        let gate = state.gate.clone();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let snapshot = client
                .connections_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let guard = gate.lock_owned().await;
            if epoch.load(Ordering::Acquire) != expected {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                aggregate(snapshot, &epoch, expected)
            })
            .await
            .map_err(|error| error.to_string())
        });
        state.task.replace(&task);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.page != Page::Home
                    || this.core_session.generation() != generation
                    || this.home.flow.epoch.load(Ordering::Acquire) != expected
                {
                    return;
                }
                this.home.flow.finish(expected, generation, result);
                cx.notify();
            });
        })
        .detach();
    }
    fn home_flow_summary(&self) -> Option<&FlowSummary> {
        (self.home.flow.generation == self.home.generation)
            .then_some(self.home.flow.summary.as_ref())
            .flatten()
    }
    fn home_flow_status(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
        let state = &self.home.flow;
        let message = if state.generation != self.home.generation {
            "home.flow.loading"
        } else if state.error.is_some() && state.summary.is_none() {
            "common.status.unavailable"
        } else if state.error.is_some() {
            "home.flow.stale"
        } else if state.summary.is_none() {
            "home.flow.loading"
        } else {
            "home.flow.scope"
        };
        div()
            .text_xs()
            .text_color(if state.error.is_some() {
                theme.warning
            } else {
                theme.muted_foreground
            })
            .child(zenclash_i18n::text(message))
    }
    pub(super) fn render_home_routes(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
        let mut content = v_flex().p_4().gap_3().child(self.home_flow_status(theme));
        if let Some(summary) = self.home_flow_summary() {
            let kinds = [Route::Proxy, Route::Direct, Route::Reject, Route::Unknown];
            let colors = [
                theme.primary,
                theme.info,
                theme.danger,
                theme.muted_foreground,
            ];
            let total = summary.routes.iter().sum::<u64>();
            if total > 0 {
                let slices = kinds
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| summary.routes[*index] > 0)
                    .map(|(index, kind)| {
                        (kind.label(), summary.routes[index] as f32, colors[index])
                    })
                    .collect::<Vec<_>>();
                let legend =
                    v_flex()
                        .gap_2()
                        .children(kinds.iter().enumerate().map(|(index, kind)| {
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(div().size_2().rounded_full().bg(colors[index]))
                                .child(kind.label())
                                .child(format!(
                                    "{} · {:.1}%",
                                    summary.routes[index],
                                    summary.routes[index] as f64 / total as f64 * 100.
                                ))
                        }));
                content = content.child(
                    h_flex()
                        .flex_wrap()
                        .gap_4()
                        .child(
                            div().size_32().child(
                                PieChart::new(slices)
                                    .inner_radius(f32::from(theme.font_size) * 2.5)
                                    .value(|slice| slice.1)
                                    .color(|slice| slice.2),
                            ),
                        )
                        .child(legend),
                );
            } else {
                content = content.child(
                    div()
                        .text_sm()
                        .child(zenclash_i18n::text("connections.empty.active")),
                );
            }
        }
        home_card(zenclash_i18n::text("home.flow.routes"), theme)
            .flex_basis(rems(19.))
            .min_w_0()
            .child(content)
    }
    pub(super) fn render_home_recent_connections(
        &self,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        let mut content = v_flex().p_4().gap_3().child(self.home_flow_status(theme));
        if let Some(summary) = self.home_flow_summary() {
            for connection in &summary.recent {
                content =
                    content.child(
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .text_sm()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(connection.host.clone()),
                            )
                            .child(div().w_32().truncate().child(
                                if connection.process.is_empty() {
                                    zenclash_i18n::text("common.status.unknown")
                                } else {
                                    connection.process.clone()
                                },
                            ))
                            .child(div().w_16().child(connection.route.label()))
                            .child(
                                div()
                                    .w_24()
                                    .text_right()
                                    .child(format_bytes(connection.bytes)),
                            ),
                    );
            }
            if summary.recent.is_empty() {
                content = content.child(
                    div()
                        .text_sm()
                        .child(zenclash_i18n::text("home.flow.recent_unknown")),
                );
            }
        }
        content = content.child(
            h_flex().justify_end().child(
                Button::new("home-open-connections")
                    .small()
                    .ghost()
                    .label(zenclash_i18n::text("home.flow.all_connections"))
                    .icon(IconName::ArrowRight)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(crate::app::NavigateConnections), cx)
                    }),
            ),
        );
        home_card(zenclash_i18n::text("home.flow.recent"), theme)
            .flex_basis(rems(30.))
            .min_w_0()
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext;
    #[gpui_kit::test]
    fn http_connection_cards_update_while_traffic_websocket_is_disconnected(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        home_http_fixture(cx, false);
    }

    #[gpui_kit::test]
    fn real_core_generation_change_releases_home_cache_and_rejects_old_dashboard(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        home_http_fixture(cx, true);
    }

    fn home_http_fixture(cx: &mut gpui_kit::TestAppContext, change_generation: bool) {
        use crate::pages::runtime::logs::tests::{
            log_test_services, settings_test_directory, settle_log_preferences,
        };
        use zenclash_core::{
            AppPreferences, AppPreferencesStore, CoreKind, CoreSession, MihomoClient,
            MihomoEndpoint,
        };
        let directory = settings_test_directory("home-http");
        let store = AppPreferencesStore::new(directory.join("preferences.json"));
        let preferences = AppPreferences::default();
        store.update(|saved| *saved = preferences.clone()).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                0,
            )))
            .unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = runtime.spawn(serve_connection_fixture(listener));
        let mut services = log_test_services(&runtime, &directory, &store, preferences);
        services.client = MihomoClient::new(endpoint).unwrap();
        services.core_session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            services.client.clone(),
            None,
            Vec::new(),
        )
        .unwrap();
        services.profile_service = crate::ProfileService::new(services.core_session.clone(), None);
        let status = services.operational_status.clone();
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut owner = None;
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1200.), gpui_kit::px(2600.)),
            |window, cx| {
                let page = cx.new(|cx| RuntimePage::new(Page::Home, services, window, cx));
                owner = Some(page.clone());
                gpui_kit::component::Root::new(page, window, cx)
            },
        );
        let page = owner.unwrap();
        cx.foreground_executor().clone().block_test(async {
            settle_log_preferences(cx, &runtime, &page, |page| !page.persistent_loading).await;
            cx.update(|cx| {
                page.update(cx, |page, cx| {
                    assert!(!page.traffic_monitor.snapshot().connected);
                    page.update_home_traffic_presentation(cx);
                })
            });
            settle_log_preferences(cx, &runtime, &page, |page| page.home.flow.summary.is_some())
                .await;
        });
        cx.update(|cx| {
            let page = page.read(cx);
            assert!(!page.traffic_monitor.snapshot().connected);
            let summary = page.home_flow_summary().unwrap();
            assert_eq!(summary.routes, [1, 0, 0, 0]);
            assert_eq!(summary.recent[0].bytes, 80);
            assert_eq!(summary.recent[0].host, "fixture.invalid");
            assert!(page.home.flow.error.is_none());
        });
        if change_generation {
            server.abort();
            let core = cx.update(|cx| page.read(cx).core_session.clone());
            let previous = core.generation();
            cx.foreground_executor().clone().block_test(async {
                runtime
                    .spawn(async move { core.shutdown().await })
                    .await
                    .unwrap()
                    .unwrap();
            });
            cx.update(|cx| {
                page.update(cx, |page, _| {
                    assert!(page.core_session.generation() > previous);
                    page.reconcile_home_generation();
                    assert_eq!(page.home.generation, page.core_session.generation());
                    assert!(page.home.flow.summary.is_none());
                    assert!(page.home.projection.is_none());
                    assert!(
                        page.home
                            .chart
                            .sparkline(0, page.home.generation)
                            .is_empty()
                    );
                    assert!(
                        page.home
                            .chart
                            .sparkline(2, page.home.generation)
                            .is_empty()
                    );
                    assert_ne!(page.data_runtime_version, page.home.generation);
                    page.prepare_home_projection();
                    assert!(page.home.projection.is_none());
                    assert!(page.home_flow_summary().is_none());
                })
            });
        }
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .unwrap();
        server.abort();
        status.stop();
        runtime.shutdown_timeout(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(directory);
    }

    async fn serve_connection_fixture(listener: tokio::net::TcpListener) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0; 2_048];
            let Ok(length) = socket.read(&mut request).await else {
                continue;
            };
            let body = if String::from_utf8_lossy(&request[..length])
                .starts_with("GET /connections ")
            {
                r#"{"connections":[{"id":"fixture","metadata":{"host":"fixture.invalid","process":"fixture.exe"},"upload":30,"download":50,"start":"2026-10-01T00:00:00Z","chains":["HK","Proxy"]}],"memory":4096}"#
            } else {
                r#"{"mixed-port":7890,"mode":"rule","proxies":{}}"#
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    }

    #[test]
    fn failed_refresh_preserves_last_snapshot_and_cancel_rejects_late_results() {
        let mut state = HomeFlowState::default();
        state.generation = 1;
        state.in_flight = true;
        let initial = FlowSummary {
            routes: [3, 2, 1, 0],
            ..FlowSummary::default()
        };
        assert!(state.finish(0, 1, Ok(Some(initial))));
        state.in_flight = true;
        assert!(state.finish(0, 1, Err("offline".into())));
        assert_eq!(state.summary.as_ref().unwrap().routes, [3, 2, 1, 0]);
        assert_eq!(state.error.as_deref(), Some("offline"));
        assert!(!state.in_flight);
        state.release();
        assert!(state.summary.is_none());
        assert!(!state.finish(0, 1, Ok(Some(FlowSummary::default()))));
        assert!(state.summary.is_none());
        assert!(!state.finish(1, 2, Ok(Some(FlowSummary::default()))));
    }

    #[test]
    fn flow_uses_connection_totals_real_terminal_chain_and_newest_start() {
        let mut connections = Vec::new();
        for index in 0..9 {
            let mut connection = Connection {
                upload: 10,
                download: 20,
                start: format!("2026-10-01T00:00:{index:02}Z"),
                ..Connection::default()
            };
            connection.metadata.process = format!("process-{index}");
            connection.metadata.host = format!("host-{index}");
            connection.chains = vec![
                match index {
                    0 => "DIRECT",
                    1 => "REJECT",
                    2 => "",
                    _ => "HK",
                }
                .into(),
            ];
            connections.push(connection);
        }
        let summary = aggregate(
            ConnectionsSnapshot {
                connections,
                ..ConnectionsSnapshot::default()
            },
            &AtomicU64::new(0),
            0,
        )
        .unwrap();
        assert!(
            summary
                .recent
                .iter()
                .all(|connection| connection.bytes == 30)
        );
        assert_eq!(summary.routes, [6, 1, 1, 1]);
        assert_eq!(summary.recent.len(), 2);
        assert_eq!(summary.recent[0].host, "host-8");
    }
    #[test]
    fn missing_chain_and_timestamp_remain_unknown_and_cancelled_work_is_discarded() {
        let summary = aggregate(
            ConnectionsSnapshot {
                connections: vec![Connection::default()],
                ..ConnectionsSnapshot::default()
            },
            &AtomicU64::new(0),
            0,
        )
        .unwrap();
        assert_eq!(summary.routes, [0, 0, 0, 1]);
        assert!(summary.recent.is_empty());
        assert!(aggregate(ConnectionsSnapshot::default(), &AtomicU64::new(1), 0).is_none());
    }
}
