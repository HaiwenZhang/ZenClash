use super::{DelayTestFailure, ProxyCatalog, ProxyGroup, ProxyNodeId};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

#[derive(Default)]
pub(super) struct NodeSummary {
    states: HashMap<ProxyNodeId, usize>,
    counts: [usize; 3],
}

impl NodeSummary {
    pub(super) fn new(catalog: &ProxyCatalog) -> Self {
        let groups = catalog
            .groups()
            .iter()
            .map(|group| group.name.as_str())
            .collect::<HashSet<_>>();
        let mut summary = Self::default();
        for id in catalog.groups().iter().flat_map(|group| group.all.iter()) {
            let Some(node) = catalog.node(id) else {
                continue;
            };
            if groups.contains(node.name.as_str())
                || matches!(
                    node.kind.as_str(),
                    "Direct" | "Reject" | "Compatible" | "Pass"
                )
                || summary.states.contains_key(id)
            {
                continue;
            }
            let state = Self::state(node.latest_delay());
            summary.states.insert(id.clone(), state);
            summary.counts[state] += 1;
        }
        summary
    }

    fn state(delay: Option<u32>) -> usize {
        match delay {
            Some(1..) => 0,
            Some(0) => 1,
            None => 2,
        }
    }

    pub(super) fn record(&mut self, id: &ProxyNodeId, delay: Option<u32>) {
        if let Some(previous) = self.states.get_mut(id) {
            let state = Self::state(delay);
            self.counts[*previous] -= 1;
            self.counts[state] += 1;
            *previous = state;
        }
    }

    #[cfg(test)]
    pub(super) fn counts(&self) -> [usize; 3] {
        self.counts
    }
}

pub(super) fn visible_group_indices(
    catalog: &ProxyCatalog,
    mode: &str,
    show_hidden: bool,
) -> Vec<usize> {
    let names = catalog
        .groups_for_mode(mode)
        .map(|group| group.name.as_str())
        .collect::<HashSet<_>>();
    catalog
        .groups()
        .iter()
        .enumerate()
        .filter(|(_, group)| names.contains(group.name.as_str()) && (show_hidden || !group.hidden))
        .map(|(index, _)| index)
        .collect()
}

pub(super) fn selected_group_index(
    catalog: &ProxyCatalog,
    visible: &[usize],
    expanded: &HashSet<String>,
) -> Option<usize> {
    visible
        .iter()
        .copied()
        .find(|&index| expanded.contains(&catalog.groups()[index].name))
        .or_else(|| visible.first().copied())
}

#[derive(Default)]
pub(super) struct GroupOrders {
    orders: RefCell<HashMap<String, CachedOrder>>,
    current: RefCell<HashMap<String, (String, Option<ProxyNodeId>)>>,
}

struct CachedOrder {
    sort: bool,
    hide: bool,
    indices: Rc<[usize]>,
}

impl GroupOrders {
    pub(super) fn prepare_current(&self, catalog: &ProxyCatalog) {
        let mut current = self.current.borrow_mut();
        current.clear();
        current.extend(catalog.groups().iter().map(|group| {
            let node = group
                .all
                .iter()
                .find(|id| id.controller_name() == group.now)
                .cloned();
            (group.name.clone(), (group.now.clone(), node))
        }));
    }

    pub(super) fn update_current(&self, catalog: &ProxyCatalog, name: &str) {
        if let Some(group) = catalog.groups().iter().find(|group| group.name == name) {
            let node = group
                .all
                .iter()
                .find(|id| id.controller_name() == group.now)
                .cloned();
            self.current
                .borrow_mut()
                .insert(name.to_owned(), (group.now.clone(), node));
        }
    }

    #[cfg(test)]
    pub(super) fn current_node(&self, group: &ProxyGroup) -> Option<ProxyNodeId> {
        self.current
            .borrow()
            .get(&group.name)
            .filter(|(name, _)| *name == group.now)
            .and_then(|(_, node)| node.clone())
    }

    pub(super) fn release_current(&self) {
        self.current.borrow_mut().clear();
    }

    pub(super) fn clear(&self) {
        self.orders.borrow_mut().clear();
    }

    pub(super) fn invalidate(&self, group: &str) {
        self.orders.borrow_mut().remove(group);
    }

    pub(super) fn invalidate_delays(&self, group: &str) {
        let mut orders = self.orders.borrow_mut();
        if orders
            .get(group)
            .is_some_and(|order| order.sort || order.hide)
        {
            orders.remove(group);
        }
    }

    pub(super) fn order(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        sort: bool,
        hide: bool,
        failures: &HashMap<ProxyNodeId, DelayTestFailure>,
    ) -> Rc<[usize]> {
        let mut orders = self.orders.borrow_mut();
        if let Some(order) = orders.get(&group.name)
            && order.sort == sort
            && order.hide == hide
        {
            return order.indices.clone();
        }
        let indices: Rc<[usize]> =
            visible_node_indices(catalog, group, sort, hide, failures).into();
        orders.insert(
            group.name.clone(),
            CachedOrder {
                sort,
                hide,
                indices: indices.clone(),
            },
        );
        indices
    }
}

pub(super) fn visible_node_indices(
    catalog: &ProxyCatalog,
    group: &ProxyGroup,
    sort: bool,
    hide: bool,
    failures: &std::collections::HashMap<ProxyNodeId, super::DelayTestFailure>,
) -> Vec<usize> {
    let mut nodes = group
        .all
        .iter()
        .enumerate()
        .filter(|(_, id)| {
            catalog.node(id).is_some_and(|proxy| {
                !hide
                    || (!failures.contains_key(*id)
                        && match proxy.latest_delay() {
                            Some(delay) => delay > 0,
                            None => proxy.alive != Some(false),
                        })
            })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if sort {
        nodes.sort_by_cached_key(|&index| {
            let id = &group.all[index];
            let proxy = catalog.node(id);
            let delay = proxy.and_then(|proxy| proxy.latest_delay());
            let failed = failures.contains_key(id)
                || delay == Some(0)
                || (delay.is_none() && proxy.is_some_and(|proxy| proxy.alive == Some(false)));
            (
                failed,
                delay.is_none(),
                delay.filter(|value| *value > 0).unwrap_or(u32::MAX),
            )
        });
    }
    nodes
}

pub(super) struct SearchProjection {
    pub(super) orders: HashMap<String, Rc<[usize]>>,
}

/// Immutable search text with atomically published, history-free delay snapshots.
pub(super) struct SearchIndex {
    groups: Vec<SearchGroup>,
    nodes: HashMap<ProxyNodeId, std::sync::Arc<SearchNode>>,
}

struct SearchGroup {
    name: String,
    nodes: Vec<(usize, std::sync::Arc<SearchNode>)>,
}

struct SearchNode {
    name: String,
    kind: String,
    controller: String,
    status: std::sync::atomic::AtomicU64,
}

const SEARCH_FAILED: u64 = 1 << 33;

fn search_status(node: &super::ProxyNode, failed: bool) -> u64 {
    let delay = node.latest_delay();
    let failed = failed || delay == Some(0) || (delay.is_none() && node.alive == Some(false));
    delay.map_or(0, |value| u64::from(value) + 1) | if failed { SEARCH_FAILED } else { 0 }
}

impl SearchIndex {
    pub(super) fn new(
        catalog: &ProxyCatalog,
        failures: &HashMap<ProxyNodeId, DelayTestFailure>,
    ) -> Self {
        let mut nodes = HashMap::new();
        let groups = catalog
            .groups()
            .iter()
            .map(|group| {
                let entries = group
                    .all
                    .iter()
                    .enumerate()
                    .filter_map(|(index, id)| {
                        let node = catalog.node(id)?;
                        let entry = nodes.entry(id.clone()).or_insert_with(|| {
                            std::sync::Arc::new(SearchNode {
                                name: node.name.to_lowercase(),
                                kind: node.kind.to_lowercase(),
                                controller: id.controller_name().to_lowercase(),
                                status: std::sync::atomic::AtomicU64::new(search_status(
                                    node,
                                    failures.contains_key(id),
                                )),
                            })
                        });
                        Some((index, entry.clone()))
                    })
                    .collect();
                SearchGroup {
                    name: group.name.clone(),
                    nodes: entries,
                }
            })
            .collect();
        Self { groups, nodes }
    }

    pub(super) fn update(&self, id: &ProxyNodeId, node: &super::ProxyNode, failed: bool) {
        if let Some(entry) = self.nodes.get(id) {
            entry.status.store(
                search_status(node, failed),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    pub(super) fn orders_cancellable(
        &self,
        query: &str,
        sort: bool,
        hide: bool,
        current: impl Fn() -> bool,
    ) -> Option<HashMap<String, Vec<usize>>> {
        let query = query.trim().to_lowercase();
        let mut orders = HashMap::new();
        for group in &self.groups {
            if !current() {
                return None;
            }
            let mut matching = Vec::new();
            for (position, (index, node)) in group.nodes.iter().enumerate() {
                if position % 64 == 0 && !current() {
                    return None;
                }
                if !query.is_empty()
                    && !node.name.contains(&query)
                    && !node.kind.contains(&query)
                    && !node.controller.contains(&query)
                {
                    continue;
                }
                let status = node.status.load(std::sync::atomic::Ordering::Relaxed);
                let failed = status & SEARCH_FAILED != 0;
                if hide && failed {
                    continue;
                }
                let delay = status & !SEARCH_FAILED;
                matching.push((
                    *index,
                    (
                        failed,
                        delay == 0,
                        if delay > 1 {
                            delay - 1
                        } else {
                            u64::from(u32::MAX)
                        },
                    ),
                ))
            }
            if !current() {
                return None;
            }
            if sort {
                matching.sort_by_key(|(_, key)| *key);
            }
            if !current() {
                return None;
            }
            orders.insert(
                group.name.clone(),
                matching.into_iter().map(|(index, _)| index).collect(),
            );
        }
        Some(orders)
    }

    #[cfg(test)]
    fn orders(&self, query: &str, sort: bool, hide: bool) -> HashMap<String, Vec<usize>> {
        self.orders_cancellable(query, sort, hide, || true).unwrap()
    }
}

#[cfg(test)]
pub(super) fn search_node_orders(
    catalog: &ProxyCatalog,
    query: &str,
    sort: bool,
    hide: bool,
    failures: &HashMap<ProxyNodeId, DelayTestFailure>,
) -> HashMap<String, Vec<usize>> {
    SearchIndex::new(catalog, failures).orders(query, sort, hide)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_summary_counts_shared_nodes_once_and_updates_measurements() {
        let nodes = vec![
            super::super::ProxyNode {
                name: "fast".into(),
                kind: "Shadowsocks".into(),
                history: vec![super::super::DelayHistory {
                    delay: 28,
                    ..Default::default()
                }],
                ..Default::default()
            },
            super::super::ProxyNode {
                name: "failed".into(),
                kind: "Shadowsocks".into(),
                history: vec![super::super::DelayHistory::default()],
                ..Default::default()
            },
            super::super::ProxyNode {
                name: "new".into(),
                kind: "Shadowsocks".into(),
                ..Default::default()
            },
            super::super::ProxyNode {
                name: "DIRECT".into(),
                kind: "Direct".into(),
                ..Default::default()
            },
            super::super::ProxyNode {
                name: "group".into(),
                kind: "Selector".into(),
                ..Default::default()
            },
        ];
        let catalog = ProxyCatalog::from_group_nodes(
            ["group", "other"]
                .into_iter()
                .map(|name| {
                    (
                        ProxyGroup {
                            name: name.into(),
                            ..Default::default()
                        },
                        nodes.clone(),
                    )
                })
                .collect(),
            7,
        );
        let mut summary = NodeSummary::new(&catalog);
        assert_eq!(summary.counts(), [1, 1, 1]);
        summary.record(&ProxyNodeId::new("new".into(), None), Some(50));
        assert_eq!(summary.counts(), [2, 1, 0]);
        summary.record(&ProxyNodeId::new("new".into(), None), Some(0));
        assert_eq!(summary.counts(), [1, 2, 0]);
        summary.record(&ProxyNodeId::new("DIRECT".into(), None), Some(1));
        assert_eq!(summary.counts(), [1, 2, 0]);
    }
    use zenclash_core::{DelayHistory, ProxyNode};

    #[test]
    fn prepared_group_indices_keep_mihomo_mode_and_hidden_semantics() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![
                (
                    ProxyGroup {
                        name: "first".into(),
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
                        name: "last".into(),
                        ..Default::default()
                    },
                    Vec::new(),
                ),
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
                .map(|name| {
                    (
                        ProxyGroup {
                            name: name.into(),
                            ..Default::default()
                        },
                        Vec::new(),
                    )
                })
                .collect(),
            0,
        );
        let selected = HashSet::from(["second".to_owned()]);
        assert_eq!(selected_group_index(&catalog, &[0, 1], &selected), Some(1));
        assert_eq!(selected_group_index(&catalog, &[2], &selected), Some(2));
        assert_eq!(selected_group_index(&catalog, &[], &selected), None);
    }

    #[test]
    fn current_node_cache_rejects_stale_selection_and_releases_with_page_owner() {
        let mut catalog = catalog("group");
        let current = catalog.groups()[0].all[1].controller_name().to_owned();
        catalog.set_group_selection("group", current);
        let orders = GroupOrders::default();
        orders.prepare_current(&catalog);
        assert_eq!(
            orders.current_node(&catalog.groups()[0]),
            Some(catalog.groups()[0].all[1].clone())
        );
        catalog.set_group_selection("group", "group-node-0".into());
        assert_eq!(orders.current_node(&catalog.groups()[0]), None);
        orders.update_current(&catalog, "group");
        assert_eq!(
            orders.current_node(&catalog.groups()[0]),
            Some(catalog.groups()[0].all[0].clone())
        );
        orders.release_current();
        assert_eq!(orders.current_node(&catalog.groups()[0]), None);
    }

    fn catalog(name: &str) -> ProxyCatalog {
        ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: name.into(),
                    ..Default::default()
                },
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
        assert!(
            search_node_orders(&catalog, "missing", false, false, &failures)["group"].is_empty()
        );
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
}
