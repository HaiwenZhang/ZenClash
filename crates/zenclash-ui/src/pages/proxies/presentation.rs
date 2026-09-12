use super::{DelayTestFailure, ProxyGroup, test_key};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

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
        group: &ProxyGroup,
        sort: bool,
        hide: bool,
        failures: &HashMap<String, DelayTestFailure>,
    ) -> Rc<[usize]> {
        let mut orders = self.0.borrow_mut();
        if let Some(order) = orders.get(&group.name)
            && order.sort == sort
            && order.hide == hide
        {
            return order.indices.clone();
        }
        let indices: Rc<[usize]> = visible_node_indices(group, sort, hide, failures).into();
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
    group: &ProxyGroup,
    sort: bool,
    hide: bool,
    failures: &std::collections::HashMap<String, super::DelayTestFailure>,
) -> Vec<usize> {
    let mut nodes = group
        .all
        .iter()
        .enumerate()
        .filter(|(_, proxy)| {
            !hide
                || (!failures.contains_key(&test_key(&group.name, &proxy.name))
                    && match proxy.latest_delay() {
                        Some(delay) => delay > 0,
                        None => proxy.alive != Some(false),
                    })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if sort {
        nodes.sort_by_cached_key(|&index| {
            let proxy = &group.all[index];
            let delay = proxy.latest_delay();
            let failed = failures.contains_key(&test_key(&group.name, &proxy.name))
                || delay == Some(0)
                || (delay.is_none() && proxy.alive == Some(false));
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

    fn group(name: &str) -> ProxyGroup {
        ProxyGroup {
            name: name.into(),
            all: [100, 10, 0]
                .into_iter()
                .enumerate()
                .map(|(index, delay)| ProxyNode {
                    name: format!("node-{index}"),
                    history: vec![DelayHistory {
                        delay,
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn delay_updates_preserve_catalog_order_but_refresh_latency_and_visibility() {
        for (sort, hide) in [(false, false), (true, false), (false, true), (true, true)] {
            let orders = GroupOrders::default();
            let mut group = group("group");
            let mut failures = HashMap::new();
            let before = orders.order(&group, sort, hide, &failures);
            group.all[0].history[0].delay = 1;
            failures.insert(test_key("group", "node-1"), DelayTestFailure::Timeout);
            orders.invalidate_delays(&group.name);
            let after = orders.order(&group, sort, hide, &failures);
            assert_eq!(&*after, visible_node_indices(&group, sort, hide, &failures));
            assert_eq!(Rc::ptr_eq(&before, &after), !sort && !hide);
        }
    }

    #[test]
    fn repaint_reuses_order_and_delay_changes_invalidate_only_the_affected_group() {
        let orders = GroupOrders::default();
        let failures = HashMap::new();
        let mut first = group("first");
        let second = group("second");
        let before = orders.order(&first, true, true, &failures);
        let unchanged = orders.order(&second, true, true, &failures);
        assert!(Rc::ptr_eq(
            &before,
            &orders.order(&first, true, true, &failures)
        ));
        first.all[0].history[0].delay = 1;
        orders.invalidate("first");
        assert_eq!(&*orders.order(&first, true, true, &failures), &[0, 1]);
        assert!(Rc::ptr_eq(
            &unchanged,
            &orders.order(&second, true, true, &failures)
        ));
    }

    #[test]
    fn changing_options_and_failures_rebuilds_the_order() {
        let orders = GroupOrders::default();
        let group = group("group");
        let mut failures = HashMap::new();
        assert_eq!(&*orders.order(&group, true, true, &failures), &[1, 0]);
        assert_eq!(&*orders.order(&group, false, false, &failures), &[0, 1, 2]);
        failures.insert(test_key("group", "node-1"), DelayTestFailure::Timeout);
        orders.invalidate("group");
        assert_eq!(&*orders.order(&group, true, true, &failures), &[0]);
    }

    #[test]
    fn replacing_catalog_removes_indices_from_the_previous_catalog() {
        let orders = GroupOrders::default();
        let mut group = group("same-name");
        let _ = orders.order(&group, true, false, &HashMap::new());
        group.all.truncate(1);
        orders.clear();
        assert_eq!(&*orders.order(&group, true, false, &HashMap::new()), &[0]);
    }
}
