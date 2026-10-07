use super::{
    CatalogTaskToken, ConnectionPolicy, Context, DelayTaskToken, ProxiesPage, ProxyDelayTarget,
    ProxyNodeId, ProxyOperations, ProxySelectionTaskToken, ProxyTestKey, ProxyVisibility,
    append_delay, apply_optimistic_selection, insert_inflight_test, remove_inflight_test,
    take_untested_group_proxies,
};
use futures_util::{StreamExt, stream};

const MAX_DELAY_TEST_CONCURRENCY: usize = 16;

impl ProxiesPage {
    pub(crate) fn refresh_localized_placeholders(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = &self.search_input {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text("proxies.design.search"), window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn ensure_search_input(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::component::input::{InputEvent, InputState};
        if self.search_input.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(zenclash_i18n::text("proxies.design.search"))
        });
        self.search_subscription =
            Some(cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().trim().to_lowercase();
                    if this.search_query != query {
                        this.search_query = query;
                        this.prepare_search(cx);
                        cx.notify();
                    }
                }
            }));
        self.search_input = Some(input);
    }

    pub(super) fn prepare_search(&mut self, cx: &mut Context<Self>) {
        self.cancel_search();
        let generation = self.search_generation;
        if self.search_query.is_empty() {
            return;
        }
        let Some(index) = self.search_index.clone() else {
            return;
        };
        let query = self.search_query.clone();
        let sort = self.sort_by_latency;
        let hide = self.hide_unavailable;
        let epoch = self.search_epoch.clone();
        let gate = self.search_gate.clone();
        let task = self.runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let guard = gate.lock_owned().await;
            if epoch.load(std::sync::atomic::Ordering::Acquire) != generation {
                return None;
            }
            tokio::task::spawn_blocking(move || {
                // Keep the gate until CPU work exits, even if the async parent is aborted.
                let _guard = guard;
                index.orders_cancellable(&query, sort, hide, || {
                    epoch.load(std::sync::atomic::Ordering::Acquire) == generation
                })
            })
            .await
            .ok()
            .flatten()
        });
        self.search_task = Some(task.abort_handle());
        self.presentation_tasks.track(&task);
        cx.spawn(async move |this, cx| {
            let Ok(Some(orders)) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if generation != this.search_generation {
                    return;
                }
                this.search_projection = Some(super::presentation::SearchProjection {
                    orders: orders
                        .into_iter()
                        .map(|(name, indices)| (name, indices.into()))
                        .collect(),
                });
                this.search_task = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_epoch
            .store(self.search_generation, std::sync::atomic::Ordering::Release);
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
        self.search_projection = None;
    }

    pub(super) fn displayed_nodes(
        &self,
        catalog: &super::ProxyCatalog,
        group: &super::ProxyGroup,
    ) -> std::rc::Rc<[usize]> {
        if !self.search_query.is_empty() {
            return self
                .search_projection
                .as_ref()
                .and_then(|projection| projection.orders.get(&group.name))
                .cloned()
                .unwrap_or_default();
        }
        self.group_orders.order(
            catalog,
            group,
            self.sort_by_latency,
            self.hide_unavailable,
            &self.test_failures,
        )
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(false, cx);
    }

    fn start_refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        if !force && (self.loading || self.operation_pending()) {
            return;
        }
        let token = self.begin_catalog_operation();
        self.loading = true;
        self.loading_token = Some(token);
        self.error = None;
        self.notice = None;
        cx.notify();

        let client = self.client.clone();
        let mode_revision = self.mode_revision;
        let visibility = if self.show_hidden {
            ProxyVisibility::IncludeHidden
        } else {
            ProxyVisibility::VisibleOnly
        };
        let show_hidden = self.show_hidden;
        let task = self.runtime.spawn(async move {
            let operations = ProxyOperations::new(client.clone());
            let (catalog, config) =
                tokio::try_join!(operations.catalog(visibility), client.runtime_config())
                    .map_err(|error| error.to_string())?;
            let indices =
                super::presentation::visible_group_indices(&catalog, &config.mode, show_hidden);
            Ok::<_, String>((catalog, config.mode, indices))
        });
        self.presentation_tasks.track(&task);

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "proxies.errors.catalog_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    if this.loading_token == Some(token) {
                        this.loading = false;
                        this.loading_token = None;
                        cx.notify();
                    }
                    return;
                }
                this.loading = false;
                this.loading_token = None;
                match result {
                    Ok((catalog, mut mode, mut indices)) => {
                        if mode_revision != this.mode_revision {
                            mode = this.outbound_mode.clone();
                            indices = super::presentation::visible_group_indices(
                                &catalog,
                                &mode,
                                this.show_hidden,
                            );
                        }
                        if this.expanded.is_empty()
                            && let Some(group) = catalog.groups_for_mode(&mode).next()
                        {
                            this.expanded.insert(group.name.clone());
                        }
                        this.install_catalog(catalog, mode, indices);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                this.prepare_search(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn install_catalog(
        &mut self,
        catalog: super::ProxyCatalog,
        mode: String,
        indices: Vec<usize>,
    ) {
        if !self.outbound_mode.eq_ignore_ascii_case(&mode) {
            self.mode_revision = self.mode_revision.wrapping_add(1);
            self.group_page_index = 0;
        }
        self.set_group_indices(indices);
        self.group_orders.clear();
        self.cancel_search();
        self.test_failures.clear();
        self.group_orders.prepare_current(&catalog);
        self.node_summary = super::presentation::NodeSummary::new(&catalog);
        self.search_index = Some(std::sync::Arc::new(super::presentation::SearchIndex::new(
            &catalog,
            &self.test_failures,
        )));
        self.catalog = Some(std::sync::Arc::new(catalog));
        self.outbound_mode = mode;
    }

    fn set_group_indices(&mut self, indices: Vec<usize>) {
        self.group_page_index = super::group_page(indices.len(), self.group_page_index).index;
        self.visible_group_indices = indices;
    }

    fn refresh_group_indices(&mut self) {
        let indices = self.catalog.as_ref().map_or_else(Vec::new, |catalog| {
            super::presentation::visible_group_indices(
                catalog,
                &self.outbound_mode,
                self.show_hidden,
            )
        });
        self.set_group_indices(indices);
    }

    pub(super) fn set_catalog_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.group_page_index = super::group_page(self.visible_group_indices.len(), page).index;
        cx.notify();
    }

    /// Invalidates an older catalog request and loads current controller state.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(true, cx);
    }

    /// Invalidates in-flight presentation work and releases the inactive catalog.
    pub(crate) fn suspend(&mut self) {
        self.presentation_tasks.cancel();
        self.cancel_search();
        self.group_progress.clear();
        self.search_index = None;
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.switching.clear();
        self.group_orders.clear();
        self.catalog = None;
        self.node_summary = Default::default();
        self.group_orders.release_current();
        self.visible_group_indices = Vec::new();
        self.group_page_index = 0;
        self.expanded.clear();
        self.testing.clear();
        self.active_testing_groups.clear();
        self.test_failures.clear();
        self.restoring_auto = None;
        self.measuring_and_restoring_auto = None;
        self.loading = false;
        self.loading_token = None;
        self.error = None;
        self.notice = None;
    }

    /// Clears profile-specific presentation state before loading a new profile.
    pub fn profile_activated(&mut self, cx: &mut Context<Self>) {
        self.profile_invalidated();
        self.start_refresh(true, cx);
    }

    /// Releases a stale profile catalog without fetching it for an inactive page.
    pub(crate) fn profile_invalidated(&mut self) {
        self.suspend();
        self.active_profile = None;
        self.show_hidden = false;
    }

    pub(super) fn set_show_hidden(&mut self, show_hidden: bool, cx: &mut Context<Self>) {
        if self.show_hidden == show_hidden || self.loading || self.operation_pending() {
            return;
        }
        self.show_hidden = show_hidden;
        self.group_page_index = 0;
        self.refresh_group_indices();
        self.group_orders.clear();
        self.expanded.clear();
        self.start_refresh(true, cx);
    }

    pub(super) fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        super::toggle_expanded_group(&mut self.expanded, name);
        if self.expanded.contains(name)
            && let Some(catalog) = &self.catalog
            && let Some(position) = self
                .visible_group_indices
                .iter()
                .position(|&index| catalog.groups()[index].name == name)
        {
            self.group_page_index = position / super::GROUPS_PER_PAGE;
        }
        if !self.expanded.contains(name) {
            self.group_orders.invalidate(name);
        }
        cx.notify();
    }

    pub(crate) fn set_outbound_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        // Reaffirming the same mode also supersedes an older controller read.
        self.mode_revision = self.mode_revision.wrapping_add(1);
        if self.outbound_mode.eq_ignore_ascii_case(mode) {
            return;
        }
        self.outbound_mode = mode.to_ascii_lowercase();
        self.group_page_index = 0;
        self.refresh_group_indices();
        self.group_orders.clear();
        self.expanded.clear();
        if let Some(catalog) = &self.catalog
            && let Some(group) = catalog.groups_for_mode(&self.outbound_mode).next()
        {
            self.expanded.insert(group.name.clone());
        }
        cx.notify();
    }

    pub(super) fn change_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.catalog_operation_pending() || self.switching.group_pending(&group) || self.loading
        {
            return;
        }
        let Some(request) = self.switching.start(group, proxy) else {
            return;
        };
        self.error = None;
        self.notice = None;
        cx.notify();

        let client = self.client.clone();
        let task_group = request.group.clone();
        let task_proxy = request.proxy.clone();
        let task = self.runtime.spawn(async move {
            ProxyOperations::new(client)
                .apply_selection(&task_group, &task_proxy, ConnectionPolicy::KeepExisting)
                .await
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(zenclash_i18n::text_with(
                    "proxies.errors.switch_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.switching.complete(&request) {
                    return;
                }
                match result {
                    Ok(receipt) => {
                        let warning = (!receipt.warnings.is_empty()).then(|| {
                            zenclash_i18n::text_with(
                                "proxies.notices.applied_with_warning",
                                &[("warning", receipt.warnings.join("; "))],
                            )
                        });
                        for warning in receipt.warnings {
                            tracing::warn!(%warning, "proxy selection completed with a warning");
                        }
                        apply_optimistic_selection(
                            &mut this.catalog,
                            &request.group,
                            &request.proxy,
                        );
                        if let Some(catalog) = this.catalog.as_deref() {
                            this.group_orders.update_current(catalog, &request.group);
                        }
                        this.error = None;
                        this.notice = warning;
                        this.reconcile_proxy_selection(request.token, request.group.clone(), cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reconcile_proxy_selection(
        &mut self,
        selection_token: ProxySelectionTaskToken,
        group: String,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_catalog_task();
        let client = self.client.clone();
        let task_group = group.clone();
        let task = self
            .runtime
            .spawn(async move { client.proxy_group_selection(&task_group).await });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(%error, "proxy catalog reconciliation task failed");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation)
                    || !selection_token.is_latest(this.switching.generation)
                {
                    return;
                }
                match result {
                    Ok(actual) if !actual.is_empty() => {
                        apply_optimistic_selection(&mut this.catalog, &group, &actual);
                        if let Some(catalog) = this.catalog.as_deref() {
                            this.group_orders.update_current(catalog, &group);
                        }
                        cx.notify();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "proxy catalog reconciliation failed");
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn restore_auto(&mut self, group: String, cx: &mut Context<Self>) {
        if self.operation_pending() || self.loading {
            return;
        }
        let token = self.begin_catalog_operation();
        self.restoring_auto = Some(group.clone());
        self.error = None;
        self.notice = None;
        cx.notify();

        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(async move { ProxyOperations::new(client).restore_auto(&group).await });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(zenclash_i18n::text_with(
                    "proxies.errors.restore_auto_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                this.restoring_auto = None;
                match result {
                    Ok(outcome) => {
                        let warning = (!outcome.warnings.is_empty()).then(|| {
                            zenclash_i18n::text_with(
                                "proxies.notices.restored_with_warning",
                                &[("warning", outcome.warnings.join("; "))],
                            )
                        });
                        for warning in outcome.warnings {
                            tracing::warn!(%warning, "automatic proxy group restored with a warning");
                        }
                        if let Some(catalog) = outcome.catalog {
                            let indices = super::presentation::visible_group_indices(&catalog, &this.outbound_mode, this.show_hidden);
                            this.install_catalog(catalog, this.outbound_mode.clone(), indices);
                        }
                        this.error = None;
                        this.notice = warning.or_else(|| {
                            Some(zenclash_i18n::text("proxies.notices.restored_auto"))
                        });
                    }
                    Err(error) => this.error = Some(error),
                }
                this.prepare_search(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn measure_group_and_restore_auto(
        &mut self,
        group: String,
        test_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.operation_pending() || self.loading || !self.testing.is_empty() {
            return;
        }
        let token = self.begin_catalog_operation();
        self.measuring_and_restoring_auto = Some(group.clone());
        self.error = None;
        self.notice = None;
        cx.notify();

        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            ProxyOperations::new(client)
                .measure_group_and_restore_auto(&group, test_url.as_deref(), 5_000)
                .await
                .map(|outcome| (group, outcome))
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(zenclash_i18n::text_with(
                    "proxies.errors.measure_restore_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                this.measuring_and_restoring_auto = None;
                match result {
                    Ok((group, outcome)) => {
                        let warning = (!outcome.selection.warnings.is_empty()).then(|| {
                            zenclash_i18n::text_with(
                                "proxies.notices.restored_with_warning",
                                &[("warning", outcome.selection.warnings.join("; "))],
                            )
                        });
                        for warning in outcome.selection.warnings {
                            tracing::warn!(%warning, "group delay completed with a readback warning");
                        }
                        if let Some(catalog) = outcome.selection.catalog {
                            let indices = super::presentation::visible_group_indices(&catalog, &this.outbound_mode, this.show_hidden);
                            this.install_catalog(catalog, this.outbound_mode.clone(), indices);
                        }
                        let results = this.catalog.as_ref().and_then(|catalog| {
                            let group = catalog.groups().iter().find(|item| item.name == group)?;
                            Some(group.all.iter().filter_map(|id| {
                                outcome.delays.get(id.controller_name()).copied().map(|delay| (id.clone(), delay))
                            }).collect::<Vec<_>>())
                        }).unwrap_or_default();
                        for (id, delay) in results { this.record_node_delay(&id, delay, delay, None); }
                        this.error = None;
                        this.notice = warning.or_else(|| {
                            Some(zenclash_i18n::text(
                                "proxies.notices.measured_and_restored_auto",
                            ))
                        });
                    }
                    Err(error) => this.error = Some(error),
                }
                this.prepare_search(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn test_proxy(
        &mut self,
        group: String,
        node: ProxyNodeId,
        test_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.proxy_selection_blocked(&group) || self.loading {
            return;
        }
        let Some(proxy) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.node(&node))
        else {
            return;
        };
        let target = ProxyDelayTarget {
            name: if node.provider().is_some() {
                proxy.name.clone()
            } else {
                node.controller_name().to_owned()
            },
            provider: node.provider().map(str::to_owned),
        };
        let Some(test_key) = self.start_node_test(&group, node.clone()) else {
            return;
        };
        let token = DelayTaskToken(self.delay_generation);
        cx.notify();

        let operations = ProxyOperations::new(self.client.clone());

        let task = self.runtime.spawn(async move {
            operations
                .measure(&target, test_url.as_deref(), 5_000)
                .await
                .map_err(|error| error.to_string())
        });
        self.presentation_tasks.track(&task);

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "proxies.errors.delay_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.finish_proxy_test(token, &group, &test_key) {
                    return;
                }
                match result {
                    Ok(result) => {
                        this.record_node_delay(&node, result.delay, result.mean_delay, None);
                    }
                    Err(error) => {
                        this.record_node_delay(
                            &node,
                            0,
                            0,
                            Some(super::DelayTestFailure::from_error(&error)),
                        );
                        this.error = Some(error);
                    }
                }
                this.prepare_search(cx);
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(super) fn start_proxy_test(&mut self, group: &str, proxy: &str) -> Option<ProxyTestKey> {
        self.start_node_test(group, ProxyNodeId::new(proxy.to_owned(), None))
    }

    fn start_node_test(&mut self, group: &str, node: ProxyNodeId) -> Option<ProxyTestKey> {
        let key = ProxyTestKey {
            group: group.to_owned(),
            node,
        };
        insert_inflight_test(
            &mut self.testing,
            &mut self.active_testing_groups,
            group,
            key.clone(),
        )
        .then_some(key)
    }

    pub(super) fn finish_proxy_test(
        &mut self,
        token: DelayTaskToken,
        group: &str,
        key: &ProxyTestKey,
    ) -> bool {
        if !token.is_current(self.delay_generation) {
            return false;
        }
        remove_inflight_test(
            &mut self.testing,
            &mut self.active_testing_groups,
            group,
            key,
        );
        true
    }

    pub(super) fn test_group(&mut self, group_name: &str, cx: &mut Context<Self>) {
        if self.proxy_selection_blocked(group_name)
            || self.loading
            || self.group_progress.contains_key(group_name)
        {
            return;
        }
        let Some(catalog) = self.catalog.as_deref() else {
            return;
        };
        let Some(group) = catalog
            .groups()
            .iter()
            .find(|group| group.name == group_name)
        else {
            return;
        };
        let group_name = group.name.clone();
        let test_url = group.test_url.clone();
        let proxies = take_untested_group_proxies(
            &mut self.testing,
            &mut self.active_testing_groups,
            catalog,
            group,
        );
        if proxies.is_empty() {
            return;
        }
        self.group_progress
            .insert(group_name.clone(), (0, proxies.len()));
        let token = DelayTaskToken(self.delay_generation);
        self.error = None;
        cx.notify();

        let operations = ProxyOperations::new(self.client.clone());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let task = self.runtime.spawn(async move {
            let mut measurements = stream::iter(proxies.into_iter().map(|(node, target)| {
                let operations = operations.clone();
                let test_url = test_url.clone();
                async move {
                    let result = operations
                        .measure(&target, test_url.as_deref(), 5_000)
                        .await
                        .map_err(|error| error.to_string());
                    (node, result)
                }
            }))
            .buffer_unordered(MAX_DELAY_TEST_CONCURRENCY);
            while let Some(result) = measurements.next().await {
                if sender.send(result).await.is_err() {
                    break;
                }
            }
        });
        self.presentation_tasks.track(&task);
        cx.spawn(async move |this, cx| {
            let mut failures = 0usize;
            let mut first_error = None;
            while let Some(batch) = receive_batch(&mut receiver, || {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
            })
            .await
            {
                let applied = this.update(cx, |this, cx| {
                    if !token.is_current(this.delay_generation) {
                        return false;
                    }
                    for (node, result) in batch {
                        let key = ProxyTestKey {
                            group: group_name.clone(),
                            node: node.clone(),
                        };
                        remove_inflight_test(
                            &mut this.testing,
                            &mut this.active_testing_groups,
                            &group_name,
                            &key,
                        );
                        if let Some(progress) = this.group_progress.get_mut(&group_name) {
                            progress.0 += 1;
                        }
                        match result {
                            Ok(result) => {
                                this.record_node_delay(&node, result.delay, result.mean_delay, None)
                            }
                            Err(error) => {
                                failures += 1;
                                this.record_node_delay(
                                    &node,
                                    0,
                                    0,
                                    Some(super::DelayTestFailure::from_error(&error)),
                                );
                                first_error.get_or_insert(error);
                            }
                        }
                    }
                    if let Some(error) = &first_error {
                        this.error = Some(zenclash_i18n::text_with(
                            "proxies.errors.group_failed_detail",
                            &[("count", failures.to_string()), ("error", error.clone())],
                        ));
                    }
                    if !this.search_query.is_empty() {
                        this.prepare_search(cx);
                    }
                    cx.notify();
                    true
                });
                if !matches!(applied, Ok(true)) {
                    return;
                }
            }
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.delay_generation) {
                    return;
                }
                this.group_progress.remove(&group_name);
                if !this.search_query.is_empty() {
                    this.prepare_search(cx);
                }
                if let Err(error) = result {
                    this.error = Some(error.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(super) fn record_delay(&mut self, group: &str, name: &str, delay: u32, mean_delay: u32) {
        let node = self.catalog.as_ref().and_then(|catalog| {
            let group = catalog.groups().iter().find(|item| item.name == group)?;
            group
                .all
                .iter()
                .find(|id| {
                    id.controller_name() == name
                        || catalog.node(id).is_some_and(|node| node.name == name)
                })
                .cloned()
        });
        if let Some(node) = node {
            self.record_node_delay(&node, delay, mean_delay, None);
        }
    }

    pub(super) fn record_node_delay(
        &mut self,
        id: &ProxyNodeId,
        delay: u32,
        mean_delay: u32,
        failure: Option<super::DelayTestFailure>,
    ) {
        let Some(catalog) = self.catalog.as_mut() else {
            return;
        };
        let catalog = std::sync::Arc::make_mut(catalog);
        let Some(node) = catalog.node_mut(id) else {
            return;
        };
        append_delay(node, delay, mean_delay);
        self.node_summary.record(id, node.latest_delay());
        if let Some(index) = &self.search_index {
            index.update(id, node, failure.is_some());
        }
        if let Some(failure) = failure {
            self.test_failures.insert(id.clone(), failure);
        } else {
            self.test_failures.remove(id);
        }
        for &index in catalog.referencing_groups(id) {
            self.group_orders
                .invalidate_delays(&catalog.groups()[index].name);
        }
    }

    fn next_catalog_task(&mut self) -> CatalogTaskToken {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        CatalogTaskToken(self.catalog_generation)
    }

    fn begin_catalog_operation(&mut self) -> CatalogTaskToken {
        self.presentation_tasks.cancel();
        self.cancel_search();
        self.group_progress.clear();
        let token = self.next_catalog_task();
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.testing.clear();
        self.active_testing_groups.clear();
        self.group_orders.clear();
        self.test_failures.clear();
        self.restoring_auto = None;
        self.measuring_and_restoring_auto = None;
        token
    }

    pub(super) fn operation_pending(&self) -> bool {
        self.switching.any_pending()
            || self.restoring_auto.is_some()
            || self.measuring_and_restoring_auto.is_some()
    }

    fn catalog_operation_pending(&self) -> bool {
        self.restoring_auto.is_some() || self.measuring_and_restoring_auto.is_some()
    }

    pub(super) fn proxy_selection_blocked(&self, group: &str) -> bool {
        self.catalog_operation_pending() || self.switching.group_pending(group)
    }
}

#[derive(Default)]
pub(super) struct PresentationTasks(Vec<tokio::task::AbortHandle>);

impl PresentationTasks {
    fn track<T>(&mut self, task: &tokio::task::JoinHandle<T>) {
        self.0.retain(|handle| !handle.is_finished());
        self.0.push(task.abort_handle());
    }

    fn cancel(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for PresentationTasks {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn receive_batch<T, D: std::future::Future<Output = ()>>(
    receiver: &mut tokio::sync::mpsc::Receiver<T>,
    delay: impl FnOnce() -> D,
) -> Option<Vec<T>> {
    let first = receiver.recv().await?;
    delay().await;
    let mut batch = vec![first];
    while batch.len() < 64 {
        let Ok(item) = receiver.try_recv() else {
            break;
        };
        batch.push(item);
    }
    Some(batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescing_delay_starts_only_after_the_first_measurement() {
        use futures_util::FutureExt;

        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        let started = std::cell::Cell::new(false);
        let mut batch = std::pin::pin!(receive_batch(&mut receiver, || {
            started.set(true);
            std::future::pending::<()>()
        }));
        assert!(batch.as_mut().now_or_never().is_none());
        assert!(!started.get());
        sender.try_send(42).unwrap();
        assert!(batch.as_mut().now_or_never().is_none());
        assert!(started.get());
    }

    #[gpui_kit::test]
    fn batch_wait_can_start_on_the_gpui_executor_without_a_tokio_runtime(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use futures_util::FutureExt;

        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        sender.try_send(42).unwrap();
        let _ = receive_batch(&mut receiver, || {
            cx.background_executor
                .timer(std::time::Duration::from_millis(50))
        })
        .now_or_never();
    }

    #[tokio::test]
    async fn batches_bound_ui_work_without_losing_results() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
        for index in 0..100 {
            sender.send(index).await.unwrap();
        }
        drop(sender);
        assert_eq!(
            receive_batch(&mut receiver, || tokio::time::sleep(
                std::time::Duration::from_millis(50)
            ))
            .await
            .unwrap(),
            (0..64).collect::<Vec<_>>()
        );
        assert_eq!(
            receive_batch(&mut receiver, || tokio::time::sleep(
                std::time::Duration::from_millis(50)
            ))
            .await
            .unwrap(),
            (64..100).collect::<Vec<_>>()
        );
        assert!(
            receive_batch(&mut receiver, || tokio::time::sleep(
                std::time::Duration::from_millis(50)
            ))
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn measurement_results_are_published_before_the_producer_finishes() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        sender.send(42).await.unwrap();
        let batch = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            receive_batch(&mut receiver, || {
                tokio::time::sleep(std::time::Duration::from_millis(50))
            }),
        )
        .await
        .unwrap();
        assert_eq!(batch, Some(vec![42]));
        assert!(!sender.is_closed());
    }

    #[tokio::test]
    async fn suspending_presentation_cancels_all_registered_work() {
        let mut tasks = PresentationTasks::default();
        let first = tokio::spawn(std::future::pending::<()>());
        let second = tokio::spawn(std::future::pending::<()>());
        tasks.track(&first);
        tasks.track(&second);
        tasks.cancel();
        assert!(first.await.unwrap_err().is_cancelled());
        assert!(second.await.unwrap_err().is_cancelled());
    }
}
