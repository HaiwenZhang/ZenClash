mod catalog_state;
pub(super) use catalog_state::ProxyCatalogPresentation;

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
        .map(|group| group.controller_name())
        .collect::<HashSet<_>>();
    catalog
        .groups()
        .iter()
        .enumerate()
        .filter(|(_, group)| {
            names.contains(group.controller_name()) && (show_hidden || !group.hidden)
        })
        .map(|(index, _)| index)
        .collect()
}

pub(super) fn selected_group_index(
    catalog: &ProxyCatalog,
    visible: &[usize],
    selected_group: Option<&str>,
) -> Option<usize> {
    visible
        .iter()
        .copied()
        .find(|&index| selected_group == Some(catalog.groups()[index].controller_name()))
        .or_else(|| visible.first().copied())
}

#[derive(Default)]
pub(super) struct GroupOrders {
    orders: RefCell<HashMap<String, CachedOrder>>,
}

struct CachedOrder {
    sort: bool,
    hide: bool,
    indices: Rc<[usize]>,
}

impl GroupOrders {
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
        if let Some(order) = orders.get(group.controller_name())
            && order.sort == sort
            && order.hide == hide
        {
            return order.indices.clone();
        }
        let indices: Rc<[usize]> =
            visible_node_indices(catalog, group, sort, hide, failures).into();
        orders.insert(
            group.controller_name().to_owned(),
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
    controller_key: String,
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
                    controller_key: group.controller_name().to_owned(),
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
                group.controller_key.clone(),
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
mod tests;
