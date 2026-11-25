use super::*;

use zenclash_core::{DelayHistory, ProxyNode};

#[test]
fn prepared_group_indices_keep_mihomo_mode_and_hidden_semantics() {
    let catalog = ProxyCatalog::from_group_nodes(
        vec![
            (ProxyGroup::new("first"), Vec::new()),
            (ProxyGroup::new("GLOBAL"), Vec::new()),
            (ProxyGroup::new("hidden").with_hidden(true), Vec::new()),
            (ProxyGroup::new("last"), Vec::new()),
        ],
        4,
    );
    assert_eq!(visible_group_indices(&catalog, "rule", false), [0, 3]);
    assert_eq!(visible_group_indices(&catalog, "rule", true), [0, 2, 3]);
    assert_eq!(visible_group_indices(&catalog, "GLOBAL", false), [1]);
    assert!(visible_group_indices(&catalog, "direct", true).is_empty());
    assert_eq!(visible_group_indices(&catalog, "unknown", true), [0, 2, 3]);
}

#[test]
fn master_detail_selection_tracks_group_identity_and_falls_back_on_page_changes() {
    let catalog = ProxyCatalog::from_group_nodes(
        ["first", "second", "third"]
            .into_iter()
            .map(|name| (ProxyGroup::new(name), Vec::new()))
            .collect(),
        0,
    );
    let selected = Some("second");
    assert_eq!(selected_group_index(&catalog, &[0, 1], selected), Some(1));
    assert_eq!(selected_group_index(&catalog, &[2], selected), Some(2));
    assert_eq!(selected_group_index(&catalog, &[], selected), None);
}

fn catalog(name: &str) -> ProxyCatalog {
    ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new(name),
            [100, 10, 0]
                .into_iter()
                .enumerate()
                .map(|(index, delay)| ProxyNode {
                    name: format!("{name}-node-{index}"),
                    history: vec![DelayHistory {
                        delay,
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
        )],
        3,
    )
}

#[test]
fn search_matches_case_insensitively_and_clear_restores_catalog_order() {
    let mut catalog = catalog("group");
    let id = catalog.groups()[0].all[1].clone();
    catalog.node_mut(&id).unwrap().kind = "Shadowsocks".into();
    let failures = HashMap::new();
    assert_eq!(
        search_node_orders(&catalog, " NODE-1 ", false, false, &failures)["group"],
        [1]
    );
    assert_eq!(
        search_node_orders(&catalog, "SHADOWSOCKS", false, false, &failures)["group"],
        [1]
    );
    assert!(search_node_orders(&catalog, "missing", false, false, &failures)["group"].is_empty());
    assert_eq!(
        search_node_orders(&catalog, "", false, false, &failures)["group"],
        [0, 1, 2]
    );
}

#[test]
fn search_preserves_latency_order_and_unavailable_filter() {
    let catalog = catalog("group");
    let failures = HashMap::from([(
        catalog.groups()[0].all[1].clone(),
        DelayTestFailure::Timeout,
    )]);
    assert_eq!(
        search_node_orders(&catalog, "node", true, false, &HashMap::new())["group"],
        [1, 0, 2]
    );
    assert_eq!(
        search_node_orders(&catalog, "node", true, true, &failures)["group"],
        [0]
    );
}

#[test]
fn lightweight_search_updates_latency_without_retaining_catalog_or_history() {
    let mut catalog = catalog("group");
    let index = SearchIndex::new(&catalog, &HashMap::new());
    let id = catalog.groups()[0].all[0].clone();
    let node = catalog.node_mut(&id).unwrap();
    node.history[0].delay = 1;
    index.update(&id, node, false);
    assert_eq!(index.orders("node", true, false)["group"], [0, 1, 2]);
    index.update(&id, node, true);
    assert_eq!(index.orders("node", true, true)["group"], [1]);
    drop(catalog);
    assert_eq!(index.orders("node-1", false, false)["group"], [1]);
}

#[test]
fn cancelled_search_stops_before_publishing_a_partial_order() {
    let index = SearchIndex::new(&catalog("group"), &HashMap::new());
    let checks = std::cell::Cell::new(0);
    let orders = index.orders_cancellable("node", true, false, || {
        checks.set(checks.get() + 1);
        checks.get() < 3
    });
    assert!(orders.is_none());
    assert_eq!(checks.get(), 3);
}

#[test]
fn delay_updates_preserve_catalog_order_but_refresh_latency_and_visibility() {
    for (sort, hide) in [(false, false), (true, false), (false, true), (true, true)] {
        let orders = GroupOrders::default();
        let mut catalog = catalog("group");
        let mut failures = HashMap::new();
        let before = orders.order(&catalog, &catalog.groups()[0], sort, hide, &failures);
        let id = catalog.groups()[0].all[0].clone();
        catalog.node_mut(&id).unwrap().history[0].delay = 1;
        failures.insert(
            catalog.groups()[0].all[1].clone(),
            DelayTestFailure::Timeout,
        );
        orders.invalidate_delays("group");
        let after = orders.order(&catalog, &catalog.groups()[0], sort, hide, &failures);
        assert_eq!(
            &*after,
            visible_node_indices(&catalog, &catalog.groups()[0], sort, hide, &failures)
        );
        assert_eq!(Rc::ptr_eq(&before, &after), !sort && !hide);
    }
}

#[test]
fn repaint_reuses_order_and_delay_changes_invalidate_only_the_affected_group() {
    let orders = GroupOrders::default();
    let failures = HashMap::new();
    let mut first = catalog("first");
    let second = catalog("second");
    let before = orders.order(&first, &first.groups()[0], true, true, &failures);
    let unchanged = orders.order(&second, &second.groups()[0], true, true, &failures);
    assert!(Rc::ptr_eq(
        &before,
        &orders.order(&first, &first.groups()[0], true, true, &failures)
    ));
    let id = first.groups()[0].all[0].clone();
    first.node_mut(&id).unwrap().history[0].delay = 1;
    orders.invalidate("first");
    assert_eq!(
        &*orders.order(&first, &first.groups()[0], true, true, &failures),
        &[0, 1]
    );
    assert!(Rc::ptr_eq(
        &unchanged,
        &orders.order(&second, &second.groups()[0], true, true, &failures)
    ));
}

#[test]
fn changing_options_and_failures_rebuilds_the_order() {
    let orders = GroupOrders::default();
    let catalog = catalog("group");
    let group = &catalog.groups()[0];
    let mut failures = HashMap::new();
    assert_eq!(
        &*orders.order(&catalog, group, true, true, &failures),
        &[1, 0]
    );
    assert_eq!(
        &*orders.order(&catalog, group, false, false, &failures),
        &[0, 1, 2]
    );
    failures.insert(group.all[1].clone(), DelayTestFailure::Timeout);
    orders.invalidate("group");
    assert_eq!(&*orders.order(&catalog, group, true, true, &failures), &[0]);
}
