use std::collections::{HashMap, HashSet};

use gpui::{
    App, Context, Focusable, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, button::Button, h_flex, progress::Progress,
    scroll::ScrollableElement, switch::Switch, v_flex,
};
use zenclash_core::{
    ConnectionPolicy, DelayHistory, MihomoClient, ProxyCatalog, ProxyDelayTarget, ProxyGroup,
    ProxyGroupBehavior, ProxyNode, ProxyOperations, ProxyVisibility,
};

mod actions;
mod view;

const MAX_LOCAL_DELAY_HISTORY: usize = 20;
const PROXIES_PER_PAGE: usize = 24;

/// Interactive proxy-group catalog backed by Mihomo's live controller state.
pub struct ProxiesPage {
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    catalog: Option<ProxyCatalog>,
    outbound_mode: String,
    expanded: HashSet<String>,
    proxy_pages: HashMap<String, usize>,
    testing: HashSet<String>,
    test_failures: HashMap<String, DelayTestFailure>,
    switching: ProxySelectionState,
    restoring_auto: Option<String>,
    measuring_and_restoring_auto: Option<String>,
    show_hidden: bool,
    loading: bool,
    loading_token: Option<CatalogTaskToken>,
    catalog_generation: u64,
    delay_generation: u64,
    error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui::FocusHandle,
}

impl ProxiesPage {
    /// Creates the page and begins its initial catalog request.
    pub fn new(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut page = Self {
            client,
            runtime,
            catalog: None,
            outbound_mode: "rule".into(),
            expanded: HashSet::new(),
            proxy_pages: HashMap::new(),
            testing: HashSet::new(),
            test_failures: HashMap::new(),
            switching: ProxySelectionState::default(),
            restoring_auto: None,
            measuring_and_restoring_auto: None,
            show_hidden: false,
            loading: false,
            loading_token: None,
            catalog_generation: 0,
            delay_generation: 0,
            error: None,
            notice: None,
            focus_handle: cx.focus_handle(),
        };
        page.refresh(cx);
        page
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
    let count = total.div_ceil(PROXIES_PER_PAGE);
    let index = requested_index.min(count.saturating_sub(1));
    let start = index * PROXIES_PER_PAGE;
    let end = (start + PROXIES_PER_PAGE).min(total);
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
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProxiesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let catalog = self.catalog.as_ref();
        let error = self.error.clone();
        let notice = self.notice.clone();

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
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
                        let groups = catalog
                            .groups_for_mode(&self.outbound_mode)
                            .filter(|group| self.show_hidden || !group.hidden)
                            .collect::<Vec<_>>();
                        if groups.is_empty() {
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
                                groups.into_iter().enumerate().map(|(index, group)| {
                                    self.render_group(index, group, &theme, cx)
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

fn test_key(group: &str, proxy: &str) -> String {
    format!("{group}\0{proxy}")
}

fn group_has_inflight_test(testing: &HashSet<String>, group: &str) -> bool {
    testing.iter().any(|key| {
        key.split_once('\0')
            .is_some_and(|(testing_group, _)| testing_group == group)
    })
}

fn take_untested_proxies(testing: &mut HashSet<String>, group: &ProxyGroup) -> Vec<ProxyNode> {
    group
        .all
        .iter()
        .filter(|proxy| testing.insert(test_key(&group.name, &proxy.name)))
        .cloned()
        .collect()
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

fn apply_optimistic_selection(catalog: &mut Option<ProxyCatalog>, group: &str, proxy: &str) {
    let Some(group) = catalog
        .as_mut()
        .and_then(|catalog| catalog.groups.iter_mut().find(|item| item.name == group))
    else {
        return;
    };
    group.now = proxy.to_owned();
    if matches!(group.behavior, ProxyGroupBehavior::Automatic { .. }) {
        group.behavior = ProxyGroupBehavior::Automatic { fixed: true };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_test_selects_only_nodes_without_an_inflight_test() {
        let mut testing = HashSet::from([test_key("Proxy", "HK")]);
        let group = ProxyGroup {
            name: "Proxy".into(),
            all: vec![
                ProxyNode {
                    name: "HK".into(),
                    ..ProxyNode::default()
                },
                ProxyNode {
                    name: "US".into(),
                    ..ProxyNode::default()
                },
            ],
            ..ProxyGroup::default()
        };

        let selected = take_untested_proxies(&mut testing, &group);

        assert_eq!(
            selected
                .iter()
                .map(|proxy| proxy.name.as_str())
                .collect::<Vec<_>>(),
            vec!["US"]
        );
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
        let mut catalog = Some(ProxyCatalog {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                now: "HK".into(),
                ..ProxyGroup::default()
            }],
            proxy_count: 2,
        });

        apply_optimistic_selection(&mut catalog, "Proxy", "US");

        assert_eq!(catalog.unwrap().groups[0].now, "US");
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
        let testing = HashSet::from([test_key("Proxy Auto", "HK")]);

        assert!(group_has_inflight_test(&testing, "Proxy Auto"));
        assert!(!group_has_inflight_test(&testing, "Proxy"));
    }
}
