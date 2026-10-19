use std::{collections::HashMap, rc::Rc, sync::Arc};

use super::{DelayTestFailure, GroupOrders, ProxyCatalog, ProxyGroup, ProxyNodeId, SearchIndex};

/// Derived facts for the page's current catalog, replaced and released with that catalog.
/// The page retains async admission and runtime identity; this state owns cache coherence.
#[derive(Default)]
pub(in crate::pages::proxies) struct ProxyCatalogPresentation {
    orders: GroupOrders,
    search: Option<Arc<SearchIndex>>,
    failures: HashMap<ProxyNodeId, DelayTestFailure>,
}

impl ProxyCatalogPresentation {
    pub(in crate::pages::proxies) fn install(&mut self, catalog: &ProxyCatalog) {
        self.orders.clear();
        self.failures.clear();
        self.search = Some(Arc::new(SearchIndex::new(catalog, &self.failures)));
    }

    pub(in crate::pages::proxies) fn release(&mut self) {
        self.orders.clear();
        self.search = None;
        self.failures.clear();
    }

    /// Beginning a catalog operation preserves its visible snapshot and search index.
    pub(in crate::pages::proxies) fn begin_operation(&mut self) {
        self.orders.clear();
        self.failures.clear();
    }

    pub(in crate::pages::proxies) fn search_index(&self) -> Option<Arc<SearchIndex>> {
        self.search.clone()
    }

    pub(in crate::pages::proxies) fn failure(&self, id: &ProxyNodeId) -> Option<&DelayTestFailure> {
        self.failures.get(id)
    }

    pub(in crate::pages::proxies) fn order(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        sort: bool,
        hide: bool,
    ) -> Rc<[usize]> {
        self.orders
            .order(catalog, group, sort, hide, &self.failures)
    }

    pub(in crate::pages::proxies) fn clear_order_cache(&self) {
        self.orders.clear();
    }

    pub(in crate::pages::proxies) fn invalidate_group(&self, controller_key: &str) {
        self.orders.invalidate(controller_key);
    }

    /// Apply one admitted delay result to the catalog and all derived display facts.
    pub(in crate::pages::proxies) fn record_delay(
        &mut self,
        catalog: &mut Arc<ProxyCatalog>,
        id: &ProxyNodeId,
        delay: u32,
        mean_delay: u32,
        failure: Option<DelayTestFailure>,
    ) {
        let catalog = Arc::make_mut(catalog);
        let Some(node) = catalog.node_mut(id) else {
            return;
        };
        super::super::append_delay(node, delay, mean_delay);
        if let Some(search) = &self.search {
            search.update(id, node, failure.is_some());
        }
        if let Some(failure) = failure {
            self.failures.insert(id.clone(), failure);
        } else {
            self.failures.remove(id);
        }
        for &group_ix in catalog.referencing_groups(id) {
            self.orders
                .invalidate_delays(catalog.groups()[group_ix].controller_name());
        }
    }

    #[cfg(test)]
    pub(in crate::pages::proxies) fn failures(&self) -> &HashMap<ProxyNodeId, DelayTestFailure> {
        &self.failures
    }
}
