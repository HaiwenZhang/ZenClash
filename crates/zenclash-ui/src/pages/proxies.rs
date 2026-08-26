use std::collections::{HashMap, HashSet};

use gpui::{
    div, prelude::FluentBuilder, px, App, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::Button, h_flex, progress::Progress, scroll::ScrollableElement, v_flex, ActiveTheme,
    Disableable, Icon, IconName, Sizable,
};
use zenclash_core::{DelayHistory, MihomoClient, ProxyCatalog, ProxyGroup, ProxyNode};

mod actions;
mod view;

const MAX_LOCAL_DELAY_HISTORY: usize = 20;

/// Interactive proxy-group catalog backed by Mihomo's live controller state.
pub struct ProxiesPage {
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    catalog: Option<ProxyCatalog>,
    outbound_mode: String,
    expanded: HashSet<String>,
    testing: HashSet<String>,
    test_failures: HashMap<String, DelayTestFailure>,
    switching: Option<(String, String)>,
    loading: bool,
    catalog_generation: u64,
    error: Option<String>,
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
            testing: HashSet::new(),
            test_failures: HashMap::new(),
            switching: None,
            loading: false,
            catalog_generation: 0,
            error: None,
            focus_handle: cx.focus_handle(),
        };
        page.refresh(cx);
        page
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogTaskToken(u64);

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

    const fn label(self) -> &'static str {
        match self {
            Self::Timeout => "超时",
            Self::Failed => "失败",
        }
    }
}

impl CatalogTaskToken {
    const fn is_current(self, generation: u64) -> bool {
        self.0 == generation
    }
}

impl Focusable for ProxiesPage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProxiesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let catalog = self.catalog.clone();
        let error = self.error.clone();

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
                    .gap_5()
                    .px_6()
                    .py_5()
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
                    .when(self.loading && catalog.is_none(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .rounded(theme.radius)
                                .bg(theme.secondary)
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("正在读取 Mihomo 代理组…"),
                        )
                    })
                    .when_some(catalog, |this, catalog| {
                        let groups = catalog
                            .groups_for_mode(&self.outbound_mode)
                            .collect::<Vec<_>>();
                        if groups.is_empty() {
                            let message = if self.outbound_mode.eq_ignore_ascii_case("direct") {
                                "当前为 DIRECT 直连模式，无需选择代理策略组。"
                            } else if self.outbound_mode.eq_ignore_ascii_case("global") {
                                "当前配置没有可用的 GLOBAL 策略组。"
                            } else {
                                "当前配置没有可用的代理组。"
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

fn test_key(group: &str, proxy: &str) -> String {
    format!("{group}\0{proxy}")
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
}
