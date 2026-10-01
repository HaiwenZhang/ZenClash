use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, button::Button, h_flex, progress::Progress,
    scroll::ScrollableElement, switch::Switch, v_flex,
};
use gpui_kit::{
    App, Context, Focusable, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use zenclash_core::{
    ConnectionPolicy, DelayHistory, MihomoClient, ProxyCatalog, ProxyDelayTarget, ProxyGroup,
    ProxyGroupBehavior, ProxyNode, ProxyNodeId, ProxyOperations, ProxyVisibility,
};

mod actions;
mod presentation;
mod view;

const MAX_LOCAL_DELAY_HISTORY: usize = 20;
const PROXIES_PER_PAGE: usize = 24;
const GROUPS_PER_PAGE: usize = 8;

/// Interactive proxy-group catalog backed by Mihomo's live controller state.
pub struct ProxiesPage {
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    catalog: Option<Arc<ProxyCatalog>>,
    visible_group_indices: Vec<usize>,
    group_page_index: usize,
    outbound_mode: String,
    mode_revision: u64,
    expanded: HashSet<String>,
    proxy_pages: HashMap<String, usize>,
    group_orders: presentation::GroupOrders,
    testing: HashMap<String, HashSet<ProxyNodeId>>,
    active_testing_groups: HashMap<String, usize>,
    group_progress: HashMap<String, (usize, usize)>,
    presentation_tasks: actions::PresentationTasks,
    test_failures: HashMap<ProxyNodeId, DelayTestFailure>,
    switching: ProxySelectionState,
    restoring_auto: Option<String>,
    measuring_and_restoring_auto: Option<String>,
    show_hidden: bool,
    sort_by_latency: bool,
    hide_unavailable: bool,
    loading: bool,
    loading_token: Option<CatalogTaskToken>,
    catalog_generation: u64,
    delay_generation: u64,
    error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui_kit::FocusHandle,
}

impl ProxiesPage {
    /// Creates the inactive page; its catalog is loaded on first presentation.
    pub fn new(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            catalog: None,
            visible_group_indices: Vec::new(),
            group_page_index: 0,
            outbound_mode: "rule".into(),
            mode_revision: 0,
            expanded: HashSet::new(),
            proxy_pages: HashMap::new(),
            group_orders: presentation::GroupOrders::default(),
            testing: HashMap::new(),
            active_testing_groups: HashMap::new(),
            group_progress: HashMap::new(),
            presentation_tasks: actions::PresentationTasks::default(),
            test_failures: HashMap::new(),
            switching: ProxySelectionState::default(),
            restoring_auto: None,
            measuring_and_restoring_auto: None,
            show_hidden: false,
            sort_by_latency: false,
            hide_unavailable: false,
            loading: false,
            loading_token: None,
            catalog_generation: 0,
            delay_generation: 0,
            error: None,
            notice: None,
            focus_handle: cx.focus_handle(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogTaskToken(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DelayTaskToken(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProxySelectionTaskToken(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProxySelectionRequest {
    group: String,
    proxy: String,
    token: ProxySelectionTaskToken,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingProxySelection {
    proxy: String,
    token: ProxySelectionTaskToken,
}

#[derive(Debug, Default)]
struct ProxySelectionState {
    generation: u64,
    pending: HashMap<String, PendingProxySelection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProxyPage {
    index: usize,
    count: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DelayTestFailure {
    Timeout,
    Failed,
}

impl DelayTestFailure {
    fn from_error(error: &str) -> Self {
        let error = error.to_ascii_lowercase();
        if error.contains("http 504") || error.contains("timeout") || error.contains("timed out") {
            Self::Timeout
        } else {
            Self::Failed
        }
    }

    fn label(self) -> String {
        match self {
            Self::Timeout => zenclash_i18n::text("proxies.status.timeout"),
            Self::Failed => zenclash_i18n::text("proxies.status.failed"),
        }
    }
}

impl CatalogTaskToken {
    const fn is_current(self, generation: u64) -> bool {
        self.0 == generation
    }
}

impl DelayTaskToken {
    const fn is_current(self, generation: u64) -> bool {
        self.0 == generation
    }
}

impl ProxySelectionTaskToken {
    const fn is_latest(self, generation: u64) -> bool {
        self.0 == generation
    }
}

impl ProxySelectionState {
    fn start(&mut self, group: String, proxy: String) -> Option<ProxySelectionRequest> {
        if self.pending.contains_key(&group) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let token = ProxySelectionTaskToken(self.generation);
        self.pending.insert(
            group.clone(),
            PendingProxySelection {
                proxy: proxy.clone(),
                token,
            },
        );
        Some(ProxySelectionRequest {
            group,
            proxy,
            token,
        })
    }

    fn complete(&mut self, request: &ProxySelectionRequest) -> bool {
        let is_current = self
            .pending
            .get(&request.group)
            .is_some_and(|pending| pending.token == request.token);
        if is_current {
            self.pending.remove(&request.group);
        }
        is_current
    }

    fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
    }

    fn any_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn group_pending(&self, group: &str) -> bool {
        self.pending.contains_key(group)
    }

    fn proxy_pending(&self, group: &str, proxy: &str) -> bool {
        self.pending
            .get(group)
            .is_some_and(|pending| pending.proxy == proxy)
    }
}

fn proxy_page(total: usize, requested_index: usize) -> ProxyPage {
    bounded_page(total, requested_index, PROXIES_PER_PAGE)
}

fn group_page(total: usize, requested_index: usize) -> ProxyPage {
    bounded_page(total, requested_index, GROUPS_PER_PAGE)
}

fn bounded_page(total: usize, requested_index: usize, page_size: usize) -> ProxyPage {
    let count = total.div_ceil(page_size);
    let index = requested_index.min(count.saturating_sub(1));
    let start = index * page_size;
    let end = (start + page_size).min(total);
    ProxyPage {
        index,
        count,
        start,
        end,
    }
}

fn toggle_expanded_group(expanded: &mut HashSet<String>, name: &str) {
    if expanded.remove(name) {
        return;
    }
    expanded.clear();
    expanded.insert(name.to_owned());
}

impl Focusable for ProxiesPage {
    fn focus_handle(&self, _: &App) -> gpui_kit::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProxiesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let catalog = self.catalog.as_ref();
        let error = self.error.clone();
        let notice = self.notice.clone();
        let groups = group_page(self.visible_group_indices.len(), self.group_page_index);

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .when(groups.count > 1, |this| {
                this.child(self.render_group_pagination(groups, &theme, cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_4()
                    .px_5()
                    .py_4()
                    .when_some(error, |this, error| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .p_3()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.danger.opacity(0.6))
                                .bg(theme.danger.opacity(0.12))
                                .text_sm()
                                .text_color(theme.danger)
                                .child(Icon::new(IconName::CircleX).size_4())
                                .child(error),
                        )
                    })
                    .when_some(notice, |this, notice| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .p_3()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.primary.opacity(0.5))
                                .bg(theme.primary.opacity(0.08))
                                .text_sm()
                                .text_color(theme.foreground)
                                .child(Icon::new(IconName::Info).size_4())
                                .child(notice),
                        )
                    })
                    .when(self.loading && catalog.is_none(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .rounded(theme.radius)
                                .bg(theme.secondary)
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(zenclash_i18n::text("proxies.loading")),
                        )
                    })
                    .when_some(catalog, |this, catalog| {
                        if self.visible_group_indices.is_empty() {
                            let message = if self.outbound_mode.eq_ignore_ascii_case("direct") {
                                zenclash_i18n::text("proxies.empty.direct")
                            } else if self.outbound_mode.eq_ignore_ascii_case("global") {
                                zenclash_i18n::text("proxies.empty.global")
                            } else {
                                zenclash_i18n::text("proxies.empty.rule")
                            };
                            this.child(
                                div()
                                    .p_4()
                                    .rounded(theme.radius)
                                    .bg(theme.secondary)
                                    .text_sm()
                                    .child(message),
                            )
                        } else {
                            this.children(
                                self.visible_group_indices[groups.start..groups.end]
                                    .iter()
                                    .map(|&index| {
                                        let group = &catalog.groups()[index];
                                        self.render_group(
                                            catalog,
                                            group,
                                            self.active_testing_groups.contains_key(&group.name),
                                            &theme,
                                            cx,
                                        )
                                    }),
                            )
                        }
                    }),
            )
    }
}

fn group_allows_manual_selection(behavior: &ProxyGroupBehavior) -> bool {
    matches!(
        behavior,
        ProxyGroupBehavior::Selector | ProxyGroupBehavior::Automatic { .. }
    )
}

fn group_has_unique_current(behavior: &ProxyGroupBehavior) -> bool {
    !matches!(behavior, ProxyGroupBehavior::LoadBalance)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProxyTestKey {
    group: String,
    node: ProxyNodeId,
}

#[cfg(test)]
fn test_key(group: &str, proxy: &str) -> ProxyTestKey {
    ProxyTestKey {
        group: group.to_owned(),
        node: ProxyNodeId::new(proxy.to_owned(), None),
    }
}

fn insert_inflight_test(
    testing: &mut HashMap<String, HashSet<ProxyNodeId>>,
    groups: &mut HashMap<String, usize>,
    group: &str,
    key: ProxyTestKey,
) -> bool {
    if key.group != group {
        return false;
    }
    if !testing
        .entry(group.to_owned())
        .or_default()
        .insert(key.node)
    {
        return false;
    }
    *groups.entry(group.to_owned()).or_default() += 1;
    true
}

fn remove_inflight_test(
    testing: &mut HashMap<String, HashSet<ProxyNodeId>>,
    groups: &mut HashMap<String, usize>,
    group: &str,
    key: &ProxyTestKey,
) {
    if key.group != group {
        return;
    }
    let Some(nodes) = testing.get_mut(group) else {
        return;
    };
    if !nodes.remove(&key.node) {
        return;
    }
    if nodes.is_empty() {
        testing.remove(group);
    }
    if let Some(count) = groups.get_mut(group) {
        *count -= 1;
        if *count == 0 {
            groups.remove(group);
        }
    }
}

fn take_untested_group_proxies(
    testing: &mut HashMap<String, HashSet<ProxyNodeId>>,
    groups: &mut HashMap<String, usize>,
    catalog: &ProxyCatalog,
    group: &ProxyGroup,
) -> Vec<(ProxyNodeId, ProxyDelayTarget)> {
    let proxies = take_untested_proxies(testing, catalog, group);
    if !proxies.is_empty() {
        *groups.entry(group.name.clone()).or_default() += proxies.len();
    }
    proxies
}

fn take_untested_proxies(
    testing: &mut HashMap<String, HashSet<ProxyNodeId>>,
    catalog: &ProxyCatalog,
    group: &ProxyGroup,
) -> Vec<(ProxyNodeId, ProxyDelayTarget)> {
    let inflight = testing.entry(group.name.clone()).or_default();
    let proxies = group
        .all
        .iter()
        .filter_map(|id| {
            let node = catalog.node(id)?;
            if !inflight.insert(id.clone()) {
                return None;
            }
            Some((
                id.clone(),
                ProxyDelayTarget {
                    name: if id.provider().is_some() {
                        node.name.clone()
                    } else {
                        id.controller_name().to_owned()
                    },
                    provider: id.provider().map(str::to_owned),
                },
            ))
        })
        .collect();
    if inflight.is_empty() {
        testing.remove(&group.name);
    }
    proxies
}

fn append_delay(proxy: &mut ProxyNode, delay: u32, mean_delay: u32) {
    proxy.history.push(DelayHistory {
        time: String::new(),
        delay,
        mean_delay,
    });
    if proxy.history.len() > MAX_LOCAL_DELAY_HISTORY {
        proxy
            .history
            .drain(..proxy.history.len() - MAX_LOCAL_DELAY_HISTORY);
    }
    proxy.alive = Some(delay > 0);
}

fn apply_optimistic_selection(catalog: &mut Option<Arc<ProxyCatalog>>, group: &str, proxy: &str) {
    if let Some(catalog) = catalog.as_mut() {
        Arc::make_mut(catalog).set_group_selection(group, proxy.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext, Entity, TestAppContext, size};

    #[gpui_kit::test]
    async fn identical_node_labels_keep_independent_keyboard_selection_and_delay_controls(
        cx: &mut TestAppContext,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        cx.executor().allow_parking();
        let catalog_json = r#"{"proxies":{"a":{"name":"HK","provider-name":"Airport A","history":[{"delay":100}]},"b":{"name":"HK","provider-name":"Airport B","history":[{"delay":77}]},"Proxy":{"type":"Selector","now":"a","all":["a","b"],"test-url":"http://127.0.0.1/"}}}"#;
        let (window, page, runtime) = open_catalog(cx, ProxyCatalog::default());
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut selected = String::new();
            let mut measured = Vec::new();
            for _ in 0..5 {
                let (mut stream, _) = tokio::time::timeout(
                    std::time::Duration::from_secs(5), listener.accept(),
                ).await.unwrap().unwrap();
                let mut headers = Vec::new();
                loop {
                    headers.push(stream.read_u8().await.unwrap());
                    if headers.ends_with(b"\r\n\r\n") { break; }
                    assert!(headers.len() <= 8192);
                }
                let headers = String::from_utf8(headers).unwrap();
                let body = if headers.starts_with("GET /proxies ") {
                    catalog_json.to_owned()
                } else if headers.starts_with("PUT /proxies/Proxy ") {
                    let length = headers.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap();
                    let mut payload = vec![0; length];
                    stream.read_exact(&mut payload).await.unwrap();
                    selected = serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["name"]
                        .as_str().unwrap().to_owned();
                    String::new()
                } else if headers.starts_with("GET /proxies/Proxy ") {
                    serde_json::json!({"now": selected}).to_string()
                } else {
                    let delay = if headers.starts_with("GET /providers/proxies/Airport%20A/HK/healthcheck?") {
                        measured.push("Airport A");
                        42
                    } else {
                        assert!(headers.starts_with("GET /providers/proxies/Airport%20B/HK/healthcheck?"), "{headers}");
                        measured.push("Airport B");
                        77
                    };
                    serde_json::json!({"delay": delay, "meanDelay": delay}).to_string()
                };
                let status = if body.is_empty() { "204 No Content" } else { "200 OK" };
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            measured.sort_unstable();
            (selected, measured)
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let fetch_client = client.clone();
        let catalog = runtime
            .spawn(async move { fetch_client.proxy_catalog().await.unwrap() })
            .await
            .unwrap();
        let id = |action: &'static str, key: &str, provider: &str| {
            let group = gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), "Proxy"));
            let node = gpui_kit::ElementId::from((group, key.to_owned()));
            gpui_kit::ElementId::from((node, provider.to_owned()))
        };
        let (select_b, test_a, test_b) = cx
            .update_window(window, |_, window, cx| {
                page.update(cx, |page, cx| {
                    page.client = client;
                    let indices = presentation::visible_group_indices(&catalog, "rule", false);
                    page.install_catalog(catalog, "rule".into(), indices);
                    page.expanded.insert("Proxy".into());
                    cx.notify();
                });
                let focus = page.read(cx).focus_handle.clone();
                window.focus(&focus, cx);
                window.render_frame(cx);
                let control = |window: &mut Window, action: &'static str, key, provider| {
                    let expected = id(action, key, provider);
                    if window.try_find(expected.clone()).is_some() {
                        expected
                    } else {
                        // Exercise the previous controls as well: the regression
                        // must fail on the selected/requested node, not a missing ID.
                        let group =
                            gpui_kit::ElementId::from((gpui_kit::ElementId::from(action), "Proxy"));
                        gpui_kit::ElementId::from((group, "HK"))
                    }
                };
                let select_b = control(window, "select-proxy", "b", "Airport B");
                let test_a = control(window, "test-proxy", "a", "Airport A");
                let test_b = control(window, "test-proxy", "b", "Airport B");
                for _ in 0..40 {
                    if window.find(select_b.clone()).focused() == Some(true) {
                        break;
                    }
                    window.press("tab", cx);
                }
                assert_eq!(window.find(select_b.clone()).focused(), Some(true));
                window.press("enter", cx);
                (select_b, test_a, test_b)
            })
            .unwrap();
        cx.run_until_parked();
        for _ in 0..1000 {
            if cx.update(|cx| page.read(cx).catalog.as_ref().unwrap().groups()[0].now == "b") {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert_eq!(page.read(cx).catalog.as_ref().unwrap().groups()[0].now, "b");
            window.render_frame(cx);
            assert_eq!(
                window.find(select_b).label(),
                Some(zenclash_i18n::text("proxies.actions.current").as_str())
            );
            window.click(test_a, cx);
            window.click(test_b, cx);
            let page = page.read(cx);
            let group = &page.catalog.as_ref().unwrap().groups()[0];
            let testing = page.testing.get("Proxy");
            assert!(
                group
                    .all
                    .iter()
                    .all(|node| testing.is_some_and(|testing| testing.contains(node))),
                "the same-label controls did not start independent provider measurements"
            );
            assert_eq!(page.active_testing_groups["Proxy"], 2);
        })
        .unwrap();
        let (selected, measured) = server.await.unwrap();
        assert_eq!(selected, "b");
        assert_eq!(measured, ["Airport A", "Airport B"]);
        for _ in 0..1000 {
            if cx.update(|cx| page.read(cx).testing.is_empty()) {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            let page = page.read(cx);
            assert!(page.testing.is_empty());
            let catalog = page.catalog.as_ref().unwrap();
            let delays = catalog.groups()[0]
                .all
                .iter()
                .map(|node| catalog.node(node).unwrap().latest_delay())
                .collect::<Vec<_>>();
            assert_eq!(delays, [Some(42), Some(77)]);
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn shared_node_delay_updates_all_groups_without_changing_another_provider(
        cx: &mut TestAppContext,
    ) {
        let shared = ProxyNode {
            name: "HK".into(),
            provider_name: Some("Airport A".into()),
            history: vec![DelayHistory {
                delay: 100,
                ..Default::default()
            }],
            ..Default::default()
        };
        let other = ProxyNode {
            provider_name: Some("Airport B".into()),
            history: vec![DelayHistory {
                delay: 77,
                ..Default::default()
            }],
            ..shared.clone()
        };
        let (window, page, _runtime) = open_catalog(
            cx,
            ProxyCatalog::from_group_nodes(
                vec![
                    (
                        ProxyGroup {
                            name: "first".into(),
                            ..Default::default()
                        },
                        vec![shared.clone()],
                    ),
                    (
                        ProxyGroup {
                            name: "second".into(),
                            ..Default::default()
                        },
                        vec![shared],
                    ),
                    (
                        ProxyGroup {
                            name: "other-provider".into(),
                            ..Default::default()
                        },
                        vec![other],
                    ),
                ],
                2,
            ),
        );
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, _| {
                let previous = Arc::clone(page.catalog.as_ref().unwrap());
                let prepared = previous
                    .groups()
                    .iter()
                    .map(|group| {
                        page.group_orders
                            .order(&previous, group, true, true, &page.test_failures)
                    })
                    .collect::<Vec<_>>();
                page.record_delay("first", "HK", 42, 42);
                let delays = page
                    .catalog
                    .as_ref()
                    .unwrap()
                    .groups()
                    .iter()
                    .map(|group| {
                        page.catalog
                            .as_ref()
                            .unwrap()
                            .node(&group.all[0])
                            .unwrap()
                            .latest_delay()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    delays,
                    [Some(42), Some(42), Some(77)],
                    "delay results diverged across references to the same provider node"
                );
                let catalog = page.catalog.as_ref().unwrap();
                for (index, group) in catalog.groups().iter().enumerate() {
                    let current =
                        page.group_orders
                            .order(catalog, group, true, true, &page.test_failures);
                    assert_eq!(std::rc::Rc::ptr_eq(&prepared[index], &current), index == 2);
                }
                let shared_id = catalog.groups()[0].all[0].clone();
                let other_id = catalog.groups()[2].all[0].clone();
                assert_eq!(previous.node(&shared_id).unwrap().latest_delay(), Some(100));
                assert!(std::ptr::eq(
                    previous.node(&other_id).unwrap(),
                    catalog.node(&other_id).unwrap()
                ));
                page.record_node_delay(&shared_id, 0, 0, Some(DelayTestFailure::Timeout));
                let catalog = page.catalog.as_ref().unwrap();
                let visible = catalog
                    .groups()
                    .iter()
                    .map(|group| {
                        page.group_orders
                            .order(catalog, group, true, true, &page.test_failures)
                    })
                    .map(|indices| indices.to_vec())
                    .collect::<Vec<_>>();
                assert_eq!(visible, [vec![], vec![], vec![0]]);
                assert!(!page.test_failures.contains_key(&other_id));
            });
            window.remove_window();
        })
        .unwrap();
    }

    fn open_catalog(
        cx: &mut TestAppContext,
        catalog: ProxyCatalog,
    ) -> (
        AnyWindowHandle,
        Entity<ProxiesPage>,
        tokio::runtime::Runtime,
    ) {
        cx.update(gpui_kit::init);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client =
            MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap();
        let mut page = None;
        let window = cx.open_window(size(px(1200.), px(1000.)), |window, cx| {
            let view = cx.new(|cx| {
                let mut page = ProxiesPage::new(client, runtime.handle().clone(), cx);
                let indices = presentation::visible_group_indices(&catalog, "rule", false);
                page.install_catalog(catalog, "rule".into(), indices);
                page
            });
            page = Some(view.clone());
            Root::new(view, window, cx)
        });
        (window.into(), page.unwrap(), runtime)
    }

    async fn assert_delayed_catalog_mode(
        cx: &mut TestAppContext,
        controller_mode: &str,
        subsequent_modes: &[&str],
        expected_mode: &str,
        expected_group: &str,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        cx.executor().allow_parking();
        let (window, page, runtime) = open_catalog(cx, ProxyCatalog::default());
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (requested, request_received) = tokio::sync::oneshot::channel();
        let (release, response_released) = tokio::sync::oneshot::channel();
        let config = serde_json::json!({"mode": controller_mode}).to_string();
        let server = runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let mut requested = Some(requested);
            let mut response_released = Some(response_released);
            let mut replies = tokio::task::JoinSet::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0_u8; 1024];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "controller request closed before its headers");
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                    assert!(request.len() <= 8192, "unexpected controller request size");
                }
                let request = String::from_utf8(request).unwrap();
                let (body, ready, wait) = if request.starts_with("GET /configs ") {
                    (config.clone(), requested.take(), response_released.take())
                } else {
                    assert!(request.starts_with("GET /proxies "), "{request}");
                    (r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]},"GLOBAL":{"name":"GLOBAL","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#.into(), None, None)
                };
                replies.spawn(async move {
                    if let Some(ready) = ready {
                        ready.send(()).unwrap();
                        wait.unwrap().await.unwrap();
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
            }
            while let Some(result) = replies.join_next().await {
                result.unwrap();
            }
        });
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                page.client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
                    format!("http://{address}"),
                    "",
                ))
                .unwrap();
                page.reload(cx);
            });
        })
        .unwrap();
        runtime
            .spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(5), request_received)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                assert!(page.loading);
                for mode in subsequent_modes {
                    page.set_outbound_mode(mode, cx);
                }
            });
        })
        .unwrap();
        release.send(()).unwrap();
        server.await.unwrap();
        for _ in 0..1000 {
            if cx.update(|cx| !page.read(cx).loading) {
                break;
            }
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(
                !page.read(cx).loading,
                "delayed controller response was not committed"
            );
            assert_eq!(page.read(cx).error, None);
            assert_eq!(page.read(cx).outbound_mode, expected_mode);
            window.render_frame(cx);
            for group in ["GLOBAL", "Proxy"] {
                assert_eq!(
                    window
                        .try_find((gpui_kit::ElementId::from("toggle-group"), group))
                        .is_some(),
                    group == expected_group,
                    "wrong group presentation after controller response"
                );
            }
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn delayed_catalog_response_discovers_controller_mode_without_new_input(
        cx: &mut TestAppContext,
    ) {
        assert_delayed_catalog_mode(cx, "global", &[], "global", "GLOBAL").await;
    }

    #[gpui_kit::test]
    async fn delayed_catalog_response_preserves_a_newer_mode(cx: &mut TestAppContext) {
        assert_delayed_catalog_mode(cx, "rule", &["global"], "global", "GLOBAL").await;
    }

    #[gpui_kit::test]
    async fn delayed_catalog_response_preserves_same_mode_reaffirmation(cx: &mut TestAppContext) {
        assert_delayed_catalog_mode(cx, "global", &["rule", "RULE"], "rule", "Proxy").await;
    }

    #[gpui_kit::test]
    async fn delayed_catalog_response_preserves_mode_after_aba_changes(cx: &mut TestAppContext) {
        assert_delayed_catalog_mode(cx, "global", &["global", "rule"], "rule", "Proxy").await;
    }

    #[gpui_kit::test]
    fn proxy_group_headers_are_bounded_and_keyboard_pages_reach_the_last_group(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let (window, page, _runtime) = open_catalog(
            cx,
            ProxyCatalog::from_group_nodes(
                (0..17)
                    .map(|index| {
                        (
                            ProxyGroup {
                                name: format!("group-{index}"),
                                ..Default::default()
                            },
                            Vec::new(),
                        )
                    })
                    .collect(),
                17,
            ),
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "group-0"));
            window.find((gpui_kit::ElementId::from("toggle-group"), "group-7"));
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "group-8"))
                    .is_none()
            );
        })
        .unwrap();
        for expected_page in 1..=2 {
            cx.update_window(window, |_, window, cx| {
                let focus = page.read(cx).focus_handle.clone();
                window.focus(&focus, cx);
                window.render_frame(cx);
                for _ in 0..40 {
                    if window.find("next-proxy-group-page").focused() == Some(true) {
                        break;
                    }
                    window.press("tab", cx);
                }
                assert_eq!(window.find("next-proxy-group-page").focused(), Some(true));
                window.press("enter", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                assert_eq!(page.read(cx).group_page_index, expected_page);
                window.render_frame(cx);
                let first = expected_page * GROUPS_PER_PAGE;
                let last = (first + GROUPS_PER_PAGE).min(17);
                for index in 0..17 {
                    assert_eq!(
                        window
                            .try_find((
                                gpui_kit::ElementId::from("toggle-group"),
                                format!("group-{index}"),
                            ))
                            .is_some(),
                        (first..last).contains(&index)
                    );
                }
            })
            .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            window.click("next-proxy-group-page", cx);
            assert_eq!(page.read(cx).group_page_index, 2);
            window.click("previous-proxy-group-page", cx);
            assert_eq!(page.read(cx).group_page_index, 1);
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "group-8"));
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn replacing_and_filtering_group_catalogs_clamps_pages_and_suspend_releases_them(
        cx: &mut TestAppContext,
    ) {
        let (window, page, _runtime) = open_catalog(
            cx,
            ProxyCatalog::from_group_nodes(
                (0..17)
                    .map(|index| {
                        (
                            ProxyGroup {
                                name: format!("old-{index}"),
                                ..Default::default()
                            },
                            Vec::new(),
                        )
                    })
                    .collect(),
                17,
            ),
        );
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| page.set_catalog_page(2, cx));
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "old-16"));
            page.update(cx, |page, cx| {
                let catalog = ProxyCatalog::from_group_nodes(
                    vec![
                        (
                            ProxyGroup {
                                name: "replacement".into(),
                                now: "HK".into(),
                                ..Default::default()
                            },
                            Vec::new(),
                        ),
                        (
                            ProxyGroup {
                                name: "hidden".into(),
                                hidden: true,
                                ..Default::default()
                            },
                            Vec::new(),
                        ),
                        (
                            ProxyGroup {
                                name: "GLOBAL".into(),
                                ..Default::default()
                            },
                            Vec::new(),
                        ),
                    ],
                    3,
                );
                let indices = presentation::visible_group_indices(&catalog, "rule", false);
                page.install_catalog(catalog, "rule".into(), indices);
                assert_eq!(page.group_page_index, 0);
                assert_eq!(page.catalog.as_ref().unwrap().groups()[0].now, "HK");
                cx.notify();
            });
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "replacement"));
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "old-16"))
                    .is_none()
            );
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "hidden"))
                    .is_none()
            );
            assert!(window.try_find("next-proxy-group-page").is_none());

            window.click("proxies-show-hidden", cx);
            assert!(page.read(cx).show_hidden);
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "hidden"));
            page.update(cx, |page, cx| page.set_outbound_mode("GLOBAL", cx));
            window.render_frame(cx);
            window.find((gpui_kit::ElementId::from("toggle-group"), "GLOBAL"));
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "replacement"))
                    .is_none()
            );
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "hidden"))
                    .is_none()
            );
            page.update(cx, |page, cx| page.set_outbound_mode("direct", cx));
            window.render_frame(cx);
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "GLOBAL"))
                    .is_none()
            );
            assert!(page.read(cx).visible_group_indices.is_empty());

            page.update(cx, |page, cx| {
                page.set_outbound_mode("rule", cx);
                assert_eq!(page.visible_group_indices, [0, 1]);
                page.suspend();
                assert!(page.catalog.is_none());
                assert!(page.visible_group_indices.is_empty());
                assert_eq!(page.group_page_index, 0);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("toggle-group"), "replacement"))
                    .is_none()
            );
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn proxy_group_headers_remain_testing_until_each_groups_last_result(cx: &mut TestAppContext) {
        let (window, page, _runtime) = open_catalog(
            cx,
            ProxyCatalog::from_group_nodes(
                ["Proxy", "Proxy Auto"]
                    .into_iter()
                    .map(|name| {
                        (
                            ProxyGroup {
                                name: name.into(),
                                ..Default::default()
                            },
                            ["HK", "JP"]
                                .into_iter()
                                .map(|name| ProxyNode {
                                    name: name.into(),
                                    ..Default::default()
                                })
                                .collect(),
                        )
                    })
                    .collect(),
                4,
            ),
        );
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                assert!(page.start_proxy_test("Proxy", "HK").is_some());
                assert!(page.start_proxy_test("Proxy", "HK").is_none());
                let group = &page.catalog.as_ref().unwrap().groups()[0];
                assert_eq!(
                    take_untested_group_proxies(
                        &mut page.testing,
                        &mut page.active_testing_groups,
                        page.catalog.as_deref().unwrap(),
                        group
                    )
                    .len(),
                    1
                );
                assert!(
                    take_untested_group_proxies(
                        &mut page.testing,
                        &mut page.active_testing_groups,
                        page.catalog.as_deref().unwrap(),
                        group
                    )
                    .is_empty()
                );
                page.start_proxy_test("Proxy Auto", "HK").unwrap();
                cx.notify();
            });
            window.render_frame(cx);
            for group in ["Proxy", "Proxy Auto"] {
                assert_eq!(
                    window
                        .find((gpui_kit::ElementId::from("test-group"), group))
                        .label(),
                    Some(zenclash_i18n::text("proxies.actions.testing").as_str())
                );
            }
            for (group, node, expected) in [
                ("Proxy", "HK", "proxies.actions.testing"),
                ("Proxy", "JP", "proxies.actions.test_all"),
                ("Proxy Auto", "HK", "proxies.actions.test_all"),
            ] {
                page.update(cx, |page, cx| {
                    let token = DelayTaskToken(page.delay_generation);
                    assert!(page.finish_proxy_test(token, group, &test_key(group, node)));
                    assert!(page.finish_proxy_test(token, group, &test_key(group, node)));
                    cx.notify();
                });
                window.render_frame(cx);
                assert_eq!(
                    window
                        .find((gpui_kit::ElementId::from("test-group"), group))
                        .label(),
                    Some(zenclash_i18n::text(expected).as_str())
                );
                if group == "Proxy" {
                    assert_eq!(
                        window
                            .find((gpui_kit::ElementId::from("test-group"), "Proxy Auto"))
                            .label(),
                        Some(zenclash_i18n::text("proxies.actions.testing").as_str())
                    );
                }
            }
            page.update(cx, |page, cx| {
                let stale = DelayTaskToken(page.delay_generation);
                let key = page.start_proxy_test("Proxy", "HK").unwrap();
                let catalog = page.catalog.take().unwrap();
                page.suspend();
                let indices = presentation::visible_group_indices(&catalog, "rule", false);
                page.install_catalog(Arc::unwrap_or_clone(catalog), "rule".into(), indices);
                page.start_proxy_test("Proxy", "HK").unwrap();
                assert!(!page.finish_proxy_test(stale, "Proxy", &key));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window
                    .find((gpui_kit::ElementId::from("test-group"), "Proxy"))
                    .label(),
                Some(zenclash_i18n::text("proxies.actions.testing").as_str())
            );
            page.update(cx, |page, cx| {
                assert!(page.finish_proxy_test(
                    DelayTaskToken(page.delay_generation),
                    "Proxy",
                    &test_key("Proxy", "HK")
                ));
                page.start_proxy_test("Proxy", "JP").unwrap();
                let stale = DelayTaskToken(page.delay_generation);
                page.reload(cx);
                assert!(!page.finish_proxy_test(stale, "Proxy", &test_key("Proxy", "JP")));
            });
            window.render_frame(cx);
            assert_eq!(
                window
                    .find((gpui_kit::ElementId::from("test-group"), "Proxy"))
                    .label(),
                Some(zenclash_i18n::text("proxies.actions.test_all").as_str())
            );
            page.update(cx, |page, _| page.suspend());
            window.remove_window();
        })
        .unwrap();
    }

    #[test]
    fn group_test_selects_only_nodes_without_an_inflight_test() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "Proxy".into(),
                    ..Default::default()
                },
                ["HK", "US"]
                    .into_iter()
                    .map(|name| ProxyNode {
                        name: name.into(),
                        ..Default::default()
                    })
                    .collect(),
            )],
            2,
        );
        let group = &catalog.groups()[0];
        let mut testing = HashMap::from([("Proxy".into(), HashSet::from([group.all[0].clone()]))]);
        let selected = take_untested_proxies(&mut testing, &catalog, group);
        assert_eq!(
            selected
                .iter()
                .map(|(_, proxy)| proxy.name.as_str())
                .collect::<Vec<_>>(),
            vec!["US"]
        );
        assert_eq!(selected[0].0, group.all[1]);
    }

    #[test]
    fn local_delay_history_discards_oldest_samples() {
        let mut proxy = ProxyNode::default();
        let sample_count = u32::try_from(MAX_LOCAL_DELAY_HISTORY + 1).expect("small test limit");
        for delay in 1..=sample_count {
            append_delay(&mut proxy, delay, delay);
        }

        assert_eq!(proxy.history.len(), MAX_LOCAL_DELAY_HISTORY);
        assert_eq!(proxy.history.first().map(|sample| sample.delay), Some(2));
        assert_eq!(proxy.latest_delay(), Some(21));
    }

    #[test]
    fn load_balance_group_has_no_manual_selection_or_unique_current() {
        assert!(!group_allows_manual_selection(
            &ProxyGroupBehavior::LoadBalance
        ));
        assert!(!group_has_unique_current(&ProxyGroupBehavior::LoadBalance));
    }

    #[test]
    fn selector_and_automatic_groups_allow_manual_selection() {
        assert!(group_allows_manual_selection(&ProxyGroupBehavior::Selector));
        assert!(group_allows_manual_selection(
            &ProxyGroupBehavior::Automatic { fixed: false }
        ));
    }

    #[test]
    fn catalog_task_token_rejects_results_from_an_older_profile_catalog() {
        let token = CatalogTaskToken(4);

        assert!(!token.is_current(5));
    }

    #[test]
    fn catalog_task_token_accepts_the_current_catalog_generation() {
        let token = CatalogTaskToken(7);

        assert!(token.is_current(7));
    }

    #[test]
    fn proxy_selection_state_tracks_different_groups_independently() {
        let mut state = ProxySelectionState::default();

        let proxy_request = state.start("Proxy".into(), "HK".into()).unwrap();
        state.start("Streaming".into(), "US".into()).unwrap();
        state.complete(&proxy_request);

        assert!(!state.group_pending("Proxy"));
        assert!(state.group_pending("Streaming"));
    }

    #[test]
    fn stale_proxy_selection_cannot_complete_a_newer_request() {
        let mut state = ProxySelectionState::default();

        let stale = state.start("Proxy".into(), "HK".into()).unwrap();
        state.clear();
        state.start("Proxy".into(), "US".into()).unwrap();

        assert!(!state.complete(&stale));
        assert!(state.proxy_pending("Proxy", "US"));
    }

    #[test]
    fn older_selection_readback_is_stale_after_a_newer_request_starts() {
        let mut state = ProxySelectionState::default();

        let older = state.start("Proxy".into(), "HK".into()).unwrap();
        state.complete(&older);
        state.start("Streaming".into(), "US".into()).unwrap();

        assert!(!older.token.is_latest(state.generation));
    }

    #[test]
    fn optimistic_selection_updates_current_member_without_replacing_catalog() {
        let mut catalog = Some(Arc::new(ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "Proxy".into(),
                    now: "HK".into(),
                    ..ProxyGroup::default()
                },
                Vec::new(),
            )],
            2,
        )));

        apply_optimistic_selection(&mut catalog, "Proxy", "US");

        assert_eq!(catalog.unwrap().groups()[0].now, "US");
    }

    #[test]
    fn delay_failures_distinguish_timeout_from_transport_failure() {
        assert_eq!(
            DelayTestFailure::from_error("Mihomo API returned HTTP 504: Timeout"),
            DelayTestFailure::Timeout
        );
        assert_eq!(
            DelayTestFailure::from_error("Mihomo API returned HTTP 503: transport error"),
            DelayTestFailure::Failed
        );
    }

    #[test]
    fn large_proxy_groups_render_at_most_one_page_of_nodes() {
        let page = proxy_page(500, 0);

        assert_eq!(page.end - page.start, PROXIES_PER_PAGE);
        assert_eq!(page.count, 21);
    }

    #[test]
    fn stale_proxy_page_is_clamped_after_catalog_shrinks() {
        let page = proxy_page(30, 20);

        assert_eq!(page.index, 1);
        assert_eq!(page.start, 24);
        assert_eq!(page.end, 30);
    }

    #[test]
    fn expanding_another_group_collapses_the_previous_group() {
        let mut expanded = HashSet::from(["Proxy".to_owned()]);

        toggle_expanded_group(&mut expanded, "Streaming");

        assert_eq!(expanded, HashSet::from(["Streaming".to_owned()]));
    }

    #[test]
    fn group_testing_state_matches_the_complete_group_name() {
        let mut testing = HashMap::new();
        let mut groups = HashMap::new();
        insert_inflight_test(
            &mut testing,
            &mut groups,
            "Proxy Auto",
            test_key("Proxy Auto", "HK"),
        );
        assert!(groups.contains_key("Proxy Auto"));
        assert!(!groups.contains_key("Proxy"));
    }

    #[test]
    fn group_stays_testing_until_its_last_measurement_finishes() {
        let mut testing = HashMap::new();
        let mut groups = HashMap::new();
        for (group, node) in [("Proxy", "HK"), ("Proxy", "JP"), ("Auto", "HK")] {
            insert_inflight_test(&mut testing, &mut groups, group, test_key(group, node));
        }
        remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "HK"));
        remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "HK"));
        assert_eq!(
            groups.keys().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from(["Proxy", "Auto"])
        );
        remove_inflight_test(&mut testing, &mut groups, "Proxy", &test_key("Proxy", "JP"));
        assert_eq!(
            groups.keys().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from(["Auto"])
        );
        remove_inflight_test(&mut testing, &mut groups, "Auto", &test_key("Auto", "HK"));
        assert!(groups.is_empty());
        assert!(testing.is_empty());
    }
}
