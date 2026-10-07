use super::*;
use gpui_kit::base::Button as CardButton;
use std::collections::{HashMap, HashSet};
use zenclash_core::ProxyNodeId;

pub(super) struct HomeProxyProjection {
    pub(super) current: CurrentProxySummary,
    switchable: bool,
    groups: Vec<HomeGroupRow>,
    nodes: Vec<HomeNodeRow>,
    all_nodes: Vec<HomeNodeRow>,
}

struct HomeGroupRow {
    name: String,
    behavior: ProxyGroupBehavior,
    current: bool,
}

struct HomeNodeRow {
    id: ProxyNodeId,
    delay: Option<u32>,
    current: bool,
    target_group: Option<String>,
}

impl HomeUiState {
    pub(super) fn prepare(&mut self, config: &RuntimeConfig, catalog: &ProxyCatalog) {
        let group = self
            .selected_group
            .as_ref()
            .and_then(|name| catalog.groups().iter().find(|group| &group.name == name))
            .or_else(|| current_proxy_group(config, catalog));
        self.selected_group = group.map(|group| group.name.clone());
        let direct = OutboundMode::from_api(&config.mode) == OutboundMode::Direct;
        let switchable = !direct && group.is_some_and(home_group_can_switch);
        let current = if direct {
            current_proxy_summary(config, catalog)
        } else {
            group.map_or_else(
                || current_proxy_summary(config, catalog),
                |group| proxy_summary_for_group(group, catalog),
            )
        };
        let node_row = |id: &ProxyNodeId, target: Option<&ProxyGroup>| HomeNodeRow {
            id: id.clone(),
            delay: catalog
                .node(id)
                .and_then(zenclash_core::ProxyNode::latest_delay),
            current: group.is_some_and(|group| {
                group.now == id.controller_name()
                    && !matches!(group.behavior, ProxyGroupBehavior::LoadBalance)
            }) && !direct,
            target_group: target.filter(|_| !direct).map(|group| group.name.clone()),
        };
        let nodes = group.map_or_else(Vec::new, |group| {
            group
                .all
                .iter()
                .filter(|id| id.controller_name() == group.now)
                .chain(
                    group
                        .all
                        .iter()
                        .filter(|id| id.controller_name() != group.now),
                )
                .map(|id| node_row(id, switchable.then_some(group)))
                .collect()
        });
        let mut targets = HashMap::new();
        for candidate in catalog
            .groups()
            .iter()
            .filter(|group| home_group_can_switch(group))
        {
            for id in candidate.all.iter() {
                targets.entry(id).or_insert(candidate);
            }
        }
        if let Some(group) = group.filter(|group| home_group_can_switch(group)) {
            for id in group.all.iter() {
                targets.insert(id, group);
            }
        }
        let mut seen = HashSet::new();
        let all_nodes = catalog
            .groups()
            .iter()
            .flat_map(|group| group.all.iter())
            .filter(|id| seen.insert((*id).clone()))
            .map(|id| node_row(id, targets.get(id).copied()))
            .collect();
        let groups = group
            .into_iter()
            .chain(
                catalog
                    .groups()
                    .iter()
                    .filter(|item| Some(&item.name) != self.selected_group.as_ref()),
            )
            .map(|item| HomeGroupRow {
                name: item.name.clone(),
                behavior: item.behavior.clone(),
                current: Some(&item.name) == self.selected_group.as_ref(),
            })
            .collect();
        self.projection = Some(HomeProxyProjection {
            current,
            switchable,
            groups,
            nodes,
            all_nodes,
        });
    }
}

// Width is measured from this card's actual content, so sidebar changes,
// display scale, translated labels and window resizing share one overflow rule.
fn visible_card_count(width_rems: f32, card_min: f32, maximum: usize) -> usize {
    ((width_rems - 4.5 - 9.5) / (card_min + 0.5))
        .floor()
        .max(1.)
        .min(maximum as f32) as usize
}

fn node_element_id(action: &'static str, id: &ProxyNodeId) -> gpui_kit::ElementId {
    let element = gpui_kit::ElementId::from((
        gpui_kit::ElementId::from(action),
        id.controller_name().to_owned(),
    ));
    match id.provider() {
        Some(provider) => (element, provider.to_owned()).into(),
        None => element,
    }
}

fn group_icon(behavior: &ProxyGroupBehavior) -> Icon {
    match behavior {
        ProxyGroupBehavior::Selector => Icon::default().path(crate::assets::GROUP_ICON_PATH),
        ProxyGroupBehavior::Automatic { .. } => Icon::new(IconName::Globe),
        ProxyGroupBehavior::LoadBalance => Icon::default().path("icons/network.svg"),
        ProxyGroupBehavior::Unknown(_) => Icon::new(IconName::Globe),
    }
}

impl RuntimePage {
    fn select_home_group(&mut self, group: String, cx: &mut Context<Self>) {
        if self.page != Page::Home || self.home.proxy_switching.is_some() {
            return;
        }
        self.home.selected_group = Some(group);
        self.home.proxy_error = None;
        self.prepare_home_projection();
        cx.notify();
    }

    fn select_home_node(&mut self, id: ProxyNodeId, cx: &mut Context<Self>) {
        if self.core_busy() || self.home.profile_switching.is_some() {
            return;
        }
        let target = self.home_proxy_projection().and_then(|projection| {
            projection
                .all_nodes
                .iter()
                .find(|node| node.id == id)?
                .target_group
                .clone()
        });
        if let Some(group) = target {
            self.home.selected_group = Some(group.clone());
            self.prepare_home_projection();
            self.change_home_proxy(group, id.controller_name().to_owned(), cx);
        }
    }

    pub(super) fn render_home_proxy(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let projection = self.home_proxy_projection();
        let busy = self.core_busy()
            || self.home.proxy_switching.is_some()
            || self.home.profile_switching.is_some();
        let mut groups = h_flex()
            .id("home-group-row")
            .test_support()
            .w_full()
            .gap_2()
            .child(
                div()
                    .w(rems(4.))
                    .flex_shrink_0()
                    .text_sm()
                    .child(zenclash_i18n::text("home.proxy.groups")),
            );
        let mut nodes = h_flex()
            .id("home-node-row")
            .test_support()
            .w_full()
            .gap_2()
            .child(
                div()
                    .w(rems(4.))
                    .flex_shrink_0()
                    .text_sm()
                    .child(zenclash_i18n::text("home.proxy.nodes")),
            );
        if let Some(projection) = projection {
            let group_count = visible_card_count(self.home.selector_width_rems, 12., 4);
            let card_width =
                ((self.home.selector_width_rems - 4. - 0.5 * (group_count + 1) as f32)
                    / (group_count + 1) as f32)
                    .max(1.);
            for group in projection.groups.iter().take(group_count) {
                let name = group.name.clone();
                let tooltip = group.name.clone();
                groups = groups.child(
                    CardButton::new((
                        gpui_kit::ElementId::from("home-group-card"),
                        group.name.clone(),
                    ))
                    .accessibility_label(group.name.clone())
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                            .build(window, cx)
                    })
                    .selected(group.current)
                    .aria_selected(group.current)
                    .disabled(busy)
                    .w(rems(card_width))
                    .flex_shrink_0()
                    .min_w_0()
                    .h(rems(3.5))
                    .px_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(if group.current {
                        theme.primary
                    } else {
                        theme.border
                    })
                    .bg(if group.current {
                        theme.list_active
                    } else {
                        theme.secondary
                    })
                    .hover(|style| style.bg(theme.list_hover))
                    .focus(|style| style.border_color(theme.ring).border_2())
                    .styles(|styles| styles.disabled(|style| style.opacity(0.6)))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(group_icon(&group.behavior).size_5())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(group.name.clone()),
                            )
                            .when(group.current, |row| {
                                row.child(
                                    Icon::new(IconName::Check)
                                        .size_4()
                                        .text_color(theme.primary),
                                )
                            }),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_home_group(name.clone(), cx)),
                    ),
                );
            }
            if projection.groups.len() > group_count {
                groups = groups.child(self.home_more_groups(busy, card_width, cx));
            }
            let node_count = group_count;
            for node in projection.nodes.iter().take(node_count) {
                nodes = nodes.child(self.home_node_card(node, busy, card_width, theme, cx));
            }
            if projection.all_nodes.len() > node_count
                || projection.all_nodes.len() > projection.nodes.len()
            {
                nodes = nodes.child(self.home_more_nodes(busy, card_width, cx));
            }
            if projection.nodes.is_empty() {
                nodes = nodes.child(div().text_sm().text_color(theme.muted_foreground).child(
                    format!(
                        "{} · {} · {}",
                        projection.current.node, projection.current.kind, projection.current.group
                    ),
                ));
            }
        } else {
            groups = groups.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("home.proxy.no_group")),
            );
            nodes = nodes.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("home.proxy.no_node")),
            );
        }
        let owner = cx.entity().downgrade();
        let content = div()
            .w_full()
            .on_children_prepainted(move |bounds, window, cx| {
                let Some(bounds) = bounds.first() else {
                    return;
                };
                let width = f32::from(bounds.size.width) / f32::from(window.rem_size());
                let owner = owner.clone();
                if owner.upgrade().is_some_and(|owner| {
                    (owner.read(cx).home.selector_width_rems - width).abs() > 0.25
                }) {
                    cx.defer(move |cx| {
                        let _ = owner.update(cx, |page, cx| {
                            page.home.selector_width_rems = width;
                            cx.notify();
                        });
                    });
                }
            })
            .child(v_flex().w_full().gap_2().child(groups).child(nodes));
        v_flex()
            .id("home-node-panel")
            .test_support()
            .w_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("home.proxy.title")),
            )
            .child(content)
            .when_some(self.home.proxy_error.as_ref(), |panel, error| {
                panel.child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(error.clone()),
                )
            })
            .into_any_element()
    }

    fn home_node_card(
        &self,
        node: &HomeNodeRow,
        busy: bool,
        card_width: f32,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = node.id.clone();
        let pending = self
            .home
            .proxy_switching
            .as_ref()
            .is_some_and(|(_, name)| name == id.controller_name());
        let (delay, _) = dashboard::latency_presentation(node.delay);
        let color = match node.delay {
            Some(0) => theme.danger,
            Some(1..=499) => theme.success,
            Some(_) => theme.warning,
            None => theme.muted_foreground,
        };
        let tooltip = node.target_group.as_ref().map_or_else(
            || format!("{} · {}", id.controller_name(), delay),
            |group| {
                zenclash_i18n::text_with(
                    "home.proxy.switch_in_group",
                    &[
                        ("group", group.clone()),
                        ("node", id.controller_name().to_owned()),
                    ],
                )
            },
        );
        CardButton::new(node_element_id("home-node-card", &id))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .accessibility_label(format!("{} · {}", id.controller_name(), delay))
            .selected(node.current)
            .aria_selected(node.current)
            .disabled(busy || node.target_group.is_none() || node.current)
            .w(rems(card_width))
            .flex_shrink_0()
            .min_w_0()
            .h(rems(3.5))
            .px_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(if node.current {
                theme.primary
            } else {
                theme.border
            })
            .bg(if node.current {
                theme.list_active
            } else {
                theme.secondary
            })
            .hover(|style| style.bg(theme.list_hover))
            .focus(|style| style.border_color(theme.ring).border_2())
            .styles(|styles| {
                styles.disabled(|style| style.opacity(if node.current { 1. } else { 0.6 }))
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .child(id.controller_name().to_owned()),
                                    )
                                    .when(node.current, |row| {
                                        row.child(
                                            div()
                                                .px_2()
                                                .rounded(theme.radius)
                                                .bg(theme.chart_3.opacity(0.12))
                                                .text_xs()
                                                .text_color(theme.primary)
                                                .child(zenclash_i18n::text("home.proxy.current")),
                                        )
                                    }),
                            )
                            .child(div().text_xs().text_color(color).child(if pending {
                                zenclash_i18n::text("proxies.actions.switching")
                            } else {
                                delay
                            })),
                    )
                    .when(node.current, |row| {
                        row.child(
                            Icon::new(IconName::Check)
                                .size_4()
                                .text_color(theme.primary),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select_home_node(id.clone(), cx)))
    }

    fn home_more_groups(
        &self,
        busy: bool,
        card_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        Button::new("home-more-groups")
            .label(zenclash_i18n::text("home.proxy.more_groups"))
            .outline()
            .dropdown_caret(true)
            .disabled(busy)
            .w(rems(card_width))
            .flex_shrink_0()
            .h(rems(3.5))
            .dropdown_menu(move |mut menu, window, cx| {
                menu = menu
                    .min_w(window.rem_size() * 15.)
                    .max_w(window.rem_size() * 26.)
                    .max_h(window.rem_size() * 22.)
                    .scrollable(true);
                if let Some(page) = owner.upgrade()
                    && let Some(projection) = page.read(cx).home_proxy_projection()
                {
                    for group in &projection.groups {
                        let name = group.name.clone();
                        let owner = owner.clone();
                        menu = menu.item(
                            PopupMenuItem::new(group.name.clone())
                                .checked(group.current)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |page, cx| {
                                        page.select_home_group(name.clone(), cx)
                                    });
                                }),
                        );
                    }
                }
                menu
            })
    }

    fn home_more_nodes(
        &self,
        busy: bool,
        card_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        Button::new("home-more-nodes")
            .label(zenclash_i18n::text("home.proxy.more_nodes"))
            .outline()
            .dropdown_caret(true)
            .disabled(busy)
            .w(rems(card_width))
            .flex_shrink_0()
            .h(rems(3.5))
            .dropdown_menu(move |mut menu, window, cx| {
                menu = menu
                    .min_w(window.rem_size() * 20.)
                    .max_w(window.rem_size() * 32.)
                    .max_h(window.rem_size() * 22.)
                    .scrollable(true);
                if let Some(page) = owner.upgrade()
                    && let Some(projection) = page.read(cx).home_proxy_projection()
                {
                    for node in &projection.all_nodes {
                        let id = node.id.clone();
                        let owner = owner.clone();
                        let group = node.target_group.as_deref().unwrap_or("");
                        let label = format!(
                            "{}  {}  · {}",
                            id.controller_name(),
                            dashboard::latency_presentation(node.delay).0,
                            group
                        );
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(node.current)
                                .disabled(
                                    node.current
                                        || node.target_group.is_none()
                                        || !projection.switchable,
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |page, cx| {
                                        page.select_home_node(id.clone(), cx)
                                    });
                                }),
                        );
                    }
                }
                menu
            })
    }
}

#[cfg(test)]
mod tests {
    use super::super::dashboard::latency_presentation;
    use super::*;
    use zenclash_core::{DelayHistory, ProxyNode};

    #[test]
    fn latency_timeout_is_distinct_from_unknown_and_success() {
        let timeout = latency_presentation(Some(0));
        assert_eq!(timeout.0, zenclash_i18n::text("common.status.timeout"));
        assert_eq!(timeout.1, 100.);
        assert_ne!(timeout, latency_presentation(None));
        assert_eq!(latency_presentation(Some(28)).0, "28 ms");
    }

    #[test]
    fn home_projection_keeps_late_current_node_and_all_overflow_candidates() {
        let nodes = (0..20)
            .map(|index| ProxyNode {
                name: format!("node-{index}"),
                history: vec![DelayHistory {
                    time: String::new(),
                    delay: 20 + index,
                    mean_delay: 0,
                }],
                ..ProxyNode::default()
            })
            .collect();
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "primary".into(),
                    now: "node-19".into(),
                    behavior: ProxyGroupBehavior::Selector,
                    ..ProxyGroup::default()
                },
                nodes,
            )],
            20,
        );
        let mut home = HomeUiState::default();
        home.prepare(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        let projection = home.projection.unwrap();
        assert_eq!(projection.current.node, "node-19");
        assert_eq!(projection.nodes.len(), 20);
        assert!(projection.switchable);
        assert_eq!(projection.nodes[0].id.controller_name(), "node-19");
        assert!(projection.nodes[0].current);
        assert_eq!(projection.nodes[0].delay, Some(39));
        assert_eq!(
            projection.nodes.iter().filter(|node| node.current).count(),
            1
        );
        assert_eq!(projection.nodes[1].id.controller_name(), "node-0");
    }

    #[test]
    fn direct_and_load_balance_do_not_offer_manual_switches() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "primary".into(),
                    behavior: ProxyGroupBehavior::LoadBalance,
                    ..ProxyGroup::default()
                },
                vec![ProxyNode {
                    name: "node".into(),
                    ..ProxyNode::default()
                }],
            )],
            1,
        );
        let mut home = HomeUiState::default();
        home.prepare(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        assert!(!home.projection.as_ref().unwrap().switchable);
        home.prepare(
            &RuntimeConfig {
                mode: "direct".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        let projection = home.projection.unwrap();
        assert!(
            projection
                .nodes
                .iter()
                .all(|node| node.target_group.is_none())
        );
        assert!(!projection.switchable);
        assert_eq!(projection.current.node, "DIRECT");
    }
    #[test]
    fn group_choice_survives_refresh_and_overflow_contains_nodes_from_every_group() {
        let catalog = ProxyCatalog::from_group_nodes(
            ["primary", "secondary"]
                .into_iter()
                .map(|name| {
                    (
                        ProxyGroup {
                            name: name.into(),
                            now: format!("{name}-node"),
                            behavior: ProxyGroupBehavior::Selector,
                            ..Default::default()
                        },
                        vec![ProxyNode {
                            name: format!("{name}-node"),
                            ..Default::default()
                        }],
                    )
                })
                .collect(),
            2,
        );
        let config = RuntimeConfig {
            mode: "rule".into(),
            ..Default::default()
        };
        let mut home = HomeUiState {
            selected_group: Some("secondary".into()),
            ..Default::default()
        };
        home.prepare(&config, &catalog);
        home.prepare(&config, &catalog);
        let projection = home.projection.as_ref().unwrap();
        assert_eq!(projection.groups[0].name, "secondary");
        assert_eq!(projection.current.node, "secondary-node");
        assert_eq!(projection.all_nodes.len(), 2);
        assert_eq!(
            projection.all_nodes[0].target_group.as_deref(),
            Some("primary")
        );
        assert_eq!(
            projection.all_nodes[1].target_group.as_deref(),
            Some("secondary")
        );
        home.prepare(&config, &ProxyCatalog::default());
        assert!(home.selected_group.is_none());
        assert!(home.projection.as_ref().unwrap().groups.is_empty());
    }

    #[test]
    fn full_node_names_and_provider_identity_survive_overflow_deduplication() {
        let nodes = ["provider-a", "provider-b"]
            .map(|provider| ProxyNode {
                name: "🇭🇰 香港 · 自定义节点".into(),
                provider_name: Some(provider.into()),
                ..Default::default()
            })
            .to_vec();
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "primary".into(),
                    now: "🇭🇰 香港 · 自定义节点".into(),
                    behavior: ProxyGroupBehavior::Selector,
                    ..Default::default()
                },
                nodes,
            )],
            2,
        );
        let mut home = HomeUiState::default();
        home.prepare(
            &RuntimeConfig {
                mode: "rule".into(),
                ..Default::default()
            },
            &catalog,
        );
        let projection = home.projection.unwrap();
        assert_eq!(projection.all_nodes.len(), 2);
        assert_eq!(
            projection.all_nodes[0].id.controller_name(),
            "🇭🇰 香港 · 自定义节点"
        );
        assert_ne!(
            node_element_id("home-node-card", &projection.all_nodes[0].id),
            node_element_id("home-node-card", &projection.all_nodes[1].id)
        );
    }
}
