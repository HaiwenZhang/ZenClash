use gpui_kit::AppContext;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, button::Button, h_flex,
    scroll::ScrollableElement, v_flex,
};
use gpui_kit::{
    App, Context, Focusable, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::FluentBuilder,
};
use zenclash_core::{
    ConnectionPolicy, CoreSession, DelayHistory, MihomoClient, ProxyCatalog, ProxyDelayTarget,
    ProxyGroup, ProxyGroupBehavior, ProxyNode, ProxyNodeId, ProxyOperations, ProxyVisibility,
};

use crate::components::mint_switch::MintSwitch as Switch;

#[cfg(test)]
use gpui_kit::px;

use crate::proxy_selection::{ProxySelectionRequest, ProxySelectionUiState};

mod actions;
mod catalog_runtime;
mod measurement;
mod node_grid;
mod presentation;
mod selection;
#[cfg(test)]
mod test_support;
mod view;

const MAX_LOCAL_DELAY_HISTORY: usize = 20;

/// Interactive proxy-group catalog backed by Mihomo's live controller state.
pub struct ProxiesPage {
    feedback_notifications: crate::components::feedback::FeedbackNotifications,
    _feedback_subscription: gpui_kit::Subscription,
    window_handle: Option<gpui_kit::AnyWindowHandle>,
    client: MihomoClient,
    core_session: Option<CoreSession>,
    runtime: tokio::runtime::Handle,
    catalog: Option<Arc<ProxyCatalog>>,
    catalog_operations: Option<ProxyOperations>,
    catalog_presentation: presentation::ProxyCatalogPresentation,
    active_profile: Option<(String, bool)>,
    visible_group_indices: Vec<usize>,
    outbound_mode: String,
    mode_revision: u64,
    selected_group: Option<String>,
    search_input: Option<gpui_kit::Entity<gpui_kit::component::input::InputState>>,
    search_subscription: Option<gpui_kit::Subscription>,
    search_query: String,
    search_generation: u64,
    search_epoch: Arc<std::sync::atomic::AtomicU64>,
    search_gate: Arc<tokio::sync::Mutex<()>>,
    search_projection: Option<presentation::SearchProjection>,
    search_task: Option<tokio::task::AbortHandle>,
    testing: HashMap<String, HashSet<ProxyNodeId>>,
    active_testing_groups: HashMap<String, usize>,
    group_progress: HashMap<String, (usize, usize)>,
    presentation_tasks: actions::PresentationTasks,
    switching: ProxySelectionUiState,
    restoring_auto: Option<String>,
    measuring_and_restoring_auto: Option<String>,
    sort_by_latency: bool,
    hide_unavailable: bool,
    loading: bool,
    presented: bool,
    loading_token: Option<CatalogTaskToken>,
    catalog_generation: u64,
    delay_generation: u64,
    error: Option<String>,
    notice: Option<String>,
    node_focus: HashMap<(String, ProxyNodeId), gpui_kit::FocusHandle>,
    details_focus: HashMap<(String, ProxyNodeId), gpui_kit::FocusHandle>,
    node_grid: node_grid::NodeGridState,
    focus_handle: gpui_kit::FocusHandle,
}

impl ProxiesPage {
    /// Creates an inactive page whose reads follow the application's session/configuration owner.
    pub fn new_with_session(
        session: CoreSession,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut page = Self::new(session.client().clone(), runtime, cx);
        page.core_session = Some(session);
        page
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(in crate::pages) fn design_validation_fixture(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let nodes = (0..36)
            .map(|index| ProxyNode {
                name: format!("Hong Kong {:02}", index + 1),
                kind: ["Shadowsocks", "VLESS", "Trojan"][index as usize % 3].into(),
                udp: index % 3 == 0,
                history: [44, 30, 36, 72, 54, 26, 44, 66, 52, 28]
                    .into_iter()
                    .enumerate()
                    .map(|(sample, delay)| DelayHistory {
                        delay: delay + index * 9,
                        time: format!("2026-10-06T15:{:02}:00+08:00", 32 + sample),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .enumerate()
            .map(|(index, mut node)| {
                if matches!(index, 6 | 19) {
                    node.history.last_mut().unwrap().delay = 0;
                } else if matches!(index, 7 | 8 | 33..=35) {
                    node.history.clear();
                }
                node
            })
            .collect::<Vec<_>>();
        let catalog = ProxyCatalog::from_group_nodes(
            [
                "节点选择",
                "自动选择",
                "流媒体",
                "备用线路",
                "国内网站",
                "负载均衡",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    ProxyGroup::new(name)
                        .with_kind(match index {
                            1 => "URLTest",
                            3 => "Fallback",
                            5 => "LoadBalance",
                            _ => "Selector",
                        })
                        .with_behavior(match index {
                            1 | 3 => ProxyGroupBehavior::Automatic { fixed: false },
                            5 => ProxyGroupBehavior::LoadBalance,
                            _ => ProxyGroupBehavior::Selector,
                        })
                        .with_current_member(nodes[0].name.clone()),
                    nodes.clone(),
                )
            })
            .collect(),
            nodes.len(),
        );
        Self::design_validation_catalog(client, runtime, catalog, "rule".into(), cx)
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(in crate::pages) fn design_validation_catalog(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        catalog: ProxyCatalog,
        mode: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let indices = presentation::visible_group_indices(&catalog, &mode, false);
        let mut page = Self::new(client, runtime, cx);
        page.install_catalog(catalog, mode, indices);
        page
    }

    /// Creates the inactive page; its catalog is loaded on first presentation.
    pub fn new(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let feedback_subscription = cx.observe_self(|this, cx| {
            this.node_grid.invalidate_measurements();
            if let Some(handle) = this.window_handle {
                let mut messages = Vec::new();
                if let Some(error) = this.error.clone().or_else(|| {
                    this.loading
                        .then(|| this.feedback_notifications.current_message("proxy-error"))
                        .flatten()
                }) {
                    messages.push(crate::components::feedback::Feedback::new(
                        "proxy-error",
                        crate::components::feedback::FeedbackKind::Error,
                        error,
                    ));
                }
                if let Some(notice) = &this.notice {
                    messages.push(crate::components::feedback::Feedback::new(
                        "proxy-notice",
                        crate::components::feedback::FeedbackKind::Info,
                        notice.clone(),
                    ));
                }
                let _ = cx.update_window(handle, |_, window, cx| {
                    this.feedback_notifications.publish(messages, window, cx);
                });
            }
        });
        Self {
            feedback_notifications: Default::default(),
            _feedback_subscription: feedback_subscription,
            window_handle: None,
            client,
            core_session: None,
            runtime,
            catalog: None,
            catalog_operations: None,
            catalog_presentation: Default::default(),
            active_profile: None,
            visible_group_indices: Vec::new(),
            outbound_mode: "rule".into(),
            mode_revision: 0,
            selected_group: None,
            search_input: None,
            search_subscription: None,
            search_query: String::new(),
            search_generation: 0,
            search_epoch: Arc::default(),
            search_gate: Arc::default(),
            search_projection: None,
            search_task: None,
            testing: HashMap::new(),
            active_testing_groups: HashMap::new(),
            group_progress: HashMap::new(),
            presentation_tasks: actions::PresentationTasks::default(),
            switching: ProxySelectionUiState::default(),
            restoring_auto: None,
            measuring_and_restoring_auto: None,
            sort_by_latency: false,
            hide_unavailable: false,
            loading: false,
            presented: false,
            loading_token: None,
            catalog_generation: 0,
            delay_generation: 0,
            error: None,
            notice: None,
            node_focus: HashMap::new(),
            details_focus: HashMap::new(),
            node_grid: Default::default(),
            focus_handle: cx.focus_handle(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogTaskToken(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DelayTaskToken(u64);

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

fn toggle_group_selection(selected_group: &mut Option<String>, controller_name: &str) {
    if selected_group.as_deref() == Some(controller_name) {
        *selected_group = None;
    } else {
        *selected_group = Some(controller_name.to_owned());
    }
}

impl Drop for ProxiesPage {
    fn drop(&mut self) {
        self.search_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
    }
}

impl Focusable for ProxiesPage {
    fn focus_handle(&self, _: &App) -> gpui_kit::FocusHandle {
        self.focus_handle.clone()
    }
}

impl ProxiesPage {
    pub(crate) fn set_active_profile(
        &mut self,
        profile: Option<&zenclash_core::ProfileRecord>,
        cx: &mut Context<Self>,
    ) {
        let identity = profile.map(|profile| (profile.name.clone(), profile.is_remote()));
        if self.active_profile != identity {
            self.active_profile = identity;
            cx.notify();
        }
    }
}

impl Render for ProxiesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_handle = Some(window.window_handle());
        self.ensure_search_input(window, cx);
        let compact = window.viewport_size().width < window.rem_size() * 68.;
        let columns = if window.viewport_size().width < window.rem_size() * 42. {
            1
        } else if compact {
            2
        } else {
            3
        };
        self.prepare_node_grid(columns, window, cx);

        let theme = cx.theme().clone();
        let catalog = self.catalog.as_ref();

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(crate::design::workspace_background(&theme))
            .child(self.render_header(&theme, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_4()
                    .px_8()
                    .py_3()
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
                            this.child(self.render_workspace(catalog, compact, columns, &theme, cx))
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
        *groups
            .entry(group.controller_name().to_owned())
            .or_default() += proxies.len();
    }
    proxies
}

fn take_untested_proxies(
    testing: &mut HashMap<String, HashSet<ProxyNodeId>>,
    catalog: &ProxyCatalog,
    group: &ProxyGroup,
) -> Vec<(ProxyNodeId, ProxyDelayTarget)> {
    let inflight = testing
        .entry(group.controller_name().to_owned())
        .or_default();
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
        testing.remove(group.controller_name());
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

#[cfg(test)]
mod tests;
