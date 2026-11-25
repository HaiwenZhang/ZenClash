use super::super::dashboard::latency_presentation;
use super::*;
use zenclash_core::{DelayHistory, ProxyNode};

#[test]
fn node_choices_exclude_nested_groups_but_keep_builtin_routes() {
    let catalog = ProxyCatalog::from_group_nodes(
        vec![
            (
                ProxyGroup::new("primary")
                    .with_current_member("automatic")
                    .with_behavior(ProxyGroupBehavior::Selector),
                ["automatic", "node", "DIRECT", "REJECT"]
                    .into_iter()
                    .map(|name| ProxyNode {
                        name: name.into(),
                        ..Default::default()
                    })
                    .collect(),
            ),
            (
                ProxyGroup::new("automatic")
                    .with_current_member("node")
                    .with_behavior(ProxyGroupBehavior::Automatic { fixed: false }),
                vec![ProxyNode {
                    name: "node".into(),
                    ..Default::default()
                }],
            ),
        ],
        4,
    );
    let mut home = HomeUiState {
        selected_group: Some("primary".into()),
        ..Default::default()
    };
    home.prepare(
        &RuntimeConfig {
            mode: "rule".into(),
            ..Default::default()
        },
        &catalog,
    );
    let projection = home.projection.unwrap();
    for rows in [&projection.nodes, &projection.all_nodes] {
        assert_eq!(
            rows.iter()
                .map(|node| node.id.controller_name())
                .collect::<Vec<_>>(),
            ["node", "DIRECT", "REJECT"],
        );
    }
    assert!(
        projection
            .groups
            .iter()
            .any(|group| group.name == "automatic")
    );
}

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
            ProxyGroup::new("primary")
                .with_current_member("node-19")
                .with_behavior(ProxyGroupBehavior::Selector),
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
            ProxyGroup::new("primary").with_behavior(ProxyGroupBehavior::LoadBalance),
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
                    ProxyGroup::new(name)
                        .with_current_member(format!("{name}-node"))
                        .with_behavior(ProxyGroupBehavior::Selector),
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
            ProxyGroup::new("primary")
                .with_current_member("🇭🇰 香港 · 自定义节点")
                .with_behavior(ProxyGroupBehavior::Selector),
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

#[test]
fn load_balance_group_keeps_other_switchable_group_nodes_enabled_and_uses_display_name() {
    let mut selector_group = ProxyGroup::new("selector-controller")
        .with_current_member("node-a")
        .with_behavior(ProxyGroupBehavior::Selector);
    selector_group.name = "Primary Selector".into();
    let catalog = ProxyCatalog::from_group_nodes(
        vec![
            (
                ProxyGroup::new("balanced").with_behavior(ProxyGroupBehavior::LoadBalance),
                vec![ProxyNode {
                    name: "lb-only".into(),
                    ..Default::default()
                }],
            ),
            (
                selector_group,
                vec![
                    ProxyNode {
                        name: "node-a".into(),
                        ..Default::default()
                    },
                    ProxyNode {
                        name: "node-b".into(),
                        ..Default::default()
                    },
                ],
            ),
        ],
        3,
    );
    let mut home = HomeUiState {
        selected_group: Some("balanced".into()),
        ..Default::default()
    };
    home.prepare(
        &RuntimeConfig {
            mode: "rule".into(),
            ..Default::default()
        },
        &catalog,
    );
    let projection = home.projection.unwrap();
    assert!(!projection.switchable);
    let lb_only = projection
        .all_nodes
        .iter()
        .find(|node| node.id.controller_name() == "lb-only")
        .unwrap();
    assert!(more_node_disabled(lb_only));
    assert!(!more_node_menu_label(lb_only).contains('·'));

    let switchable = projection
        .all_nodes
        .iter()
        .find(|node| node.id.controller_name() == "node-b")
        .unwrap();
    assert!(!more_node_disabled(switchable));
    let label = more_node_menu_label(switchable);
    assert!(label.contains("· Primary Selector"));
    assert!(!label.contains("selector-controller"));
}
