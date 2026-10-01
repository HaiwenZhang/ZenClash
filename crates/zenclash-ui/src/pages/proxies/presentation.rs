use super::{DelayTestFailure, ProxyCatalog, ProxyGroup, ProxyNodeId};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

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

#[derive(Default)]
pub(super) struct GroupOrders(RefCell<HashMap<String, CachedOrder>>);

struct CachedOrder {
    sort: bool,
    hide: bool,
    indices: Rc<[usize]>,
}

impl GroupOrders {
    pub(super) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    pub(super) fn invalidate(&self, group: &str) {
        self.0.borrow_mut().remove(group);
    }

    pub(super) fn invalidate_delays(&self, group: &str) {
        let mut orders = self.0.borrow_mut();
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
        let mut orders = self.0.borrow_mut();
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

#[cfg(test)]
mod tests {
    use super::*;
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
