use super::{CatalogTaskToken, Context, ProxiesPage, ProxyNodeId, ProxyVisibility};

pub(super) const MAX_DELAY_TEST_CONCURRENCY: usize = 16;

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
        let Some(index) = self.catalog_presentation.search_index() else {
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
                .and_then(|projection| projection.orders.get(group.controller_name()))
                .cloned()
                .unwrap_or_default();
        }
        self.catalog_presentation
            .order(catalog, group, self.sort_by_latency, self.hide_unavailable)
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(false, cx);
    }

    fn start_refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        if !force && (self.loading || self.operation_pending()) {
            return;
        }
        self.presented = true;
        let token = self.begin_catalog_operation();
        self.loading = true;
        self.loading_token = Some(token);
        self.error = None;
        self.notice = None;
        cx.notify();

        let client = self.client.clone();
        let session = self.core_session.clone();
        let mode_revision = self.mode_revision;
        let visibility = ProxyVisibility::VisibleOnly;
        let show_hidden = false;
        let read_source =
            std::sync::Arc::new(super::catalog_runtime::ProxyCatalogReadSource::default());
        let submitted_source = read_source.clone();
        let task = self.runtime.spawn(async move {
            let operations = submitted_source.capture(session, client).await?;
            let observation = operations
                .observe_workspace(visibility)
                .await
                .map_err(|error| error.to_string())?;
            let indices = super::presentation::visible_group_indices(
                observation.catalog(),
                observation.outbound_mode(),
                show_hidden,
            );
            Ok::<_, String>((observation, indices))
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
                    Ok((observation, mut indices)) => {
                        let operations = observation.operations();
                        let published = observation.publish_if_current(|catalog, mut mode| {
                            if mode_revision != this.mode_revision {
                                mode = this.outbound_mode.clone();
                                indices = super::presentation::visible_group_indices(
                                    &catalog, &mode, false,
                                );
                            }
                            if this.selected_group.is_none()
                                && let Some(group) = catalog.groups_for_mode(&mode).next()
                            {
                                this.selected_group = Some(group.controller_name().to_owned());
                            }
                            this.install_catalog(catalog, mode, indices);
                            this.catalog_operations = Some(operations);
                            this.error = None;
                        });
                        if published.is_err() {
                            this.publish_catalog_refresh_error(
                                token,
                                &read_source,
                                zenclash_core::MihomoError::StaleBinding.to_string(),
                            );
                        }
                    }
                    Err(error) => {
                        this.publish_catalog_refresh_error(token, &read_source, error);
                    }
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
        self.next_catalog_task();
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.testing.clear();
        self.active_testing_groups.clear();
        self.group_progress.clear();
        self.catalog_operations = None;
        if !self.outbound_mode.eq_ignore_ascii_case(&mode) {
            self.mode_revision = self.mode_revision.wrapping_add(1);
        }
        self.set_group_indices(indices);
        self.cancel_search();
        self.catalog_presentation.install(&catalog);
        self.catalog = Some(std::sync::Arc::new(catalog));
        self.outbound_mode = mode;
    }

    fn set_group_indices(&mut self, indices: Vec<usize>) {
        self.visible_group_indices = indices;
    }

    fn refresh_group_indices(&mut self) {
        let indices = self.catalog.as_ref().map_or_else(Vec::new, |catalog| {
            super::presentation::visible_group_indices(catalog, &self.outbound_mode, false)
        });
        self.set_group_indices(indices);
    }

    /// Invalidates an older catalog request and loads current controller state.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(true, cx);
    }

    /// Invalidates in-flight presentation work and releases the inactive catalog.
    pub(crate) fn suspend(&mut self) {
        self.presented = false;
        self.presentation_tasks.cancel();
        self.cancel_search();
        self.group_progress.clear();
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.switching.clear();
        self.catalog_presentation.release();
        self.catalog = None;
        self.catalog_operations = None;
        self.visible_group_indices = Vec::new();
        self.node_focus.clear();
        self.details_focus.clear();
        self.node_grid = Default::default();
        self.selected_group = None;
        self.testing.clear();
        self.active_testing_groups.clear();
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
    }

    pub(super) fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        super::toggle_group_selection(&mut self.selected_group, name);
        if self.selected_group.as_deref() != Some(name) {
            self.catalog_presentation.invalidate_group(name);
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
        self.refresh_group_indices();
        self.catalog_presentation.clear_order_cache();
        self.selected_group = None;
        if let Some(catalog) = &self.catalog
            && let Some(group) = catalog.groups_for_mode(&self.outbound_mode).next()
        {
            self.selected_group = Some(group.controller_name().to_owned());
        }
        cx.notify();
    }

    /// Adopts mode coordination and refreshes a visible catalog after configuration completion.
    pub(crate) fn synchronize_outbound_mode(
        &mut self,
        mode: &str,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_outbound_mode(mode, cx);
        if !pending
            && self.core_session.is_some()
            && self.presented
            && self
                .catalog_operations
                .as_ref()
                .is_none_or(|operations| operations.pin_current_runtime().is_err())
        {
            self.start_refresh(true, cx);
        }
    }

    #[cfg(test)]
    pub(super) fn record_delay(&mut self, group: &str, name: &str, delay: u32, mean_delay: u32) {
        let node = self.catalog.as_ref().and_then(|catalog| {
            let group = catalog
                .groups()
                .iter()
                .find(|item| item.controller_name() == group)?;
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
        self.catalog_presentation
            .record_delay(catalog, id, delay, mean_delay, failure);
    }

    fn next_catalog_task(&mut self) -> CatalogTaskToken {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        CatalogTaskToken(self.catalog_generation)
    }

    pub(super) fn begin_catalog_operation(&mut self) -> CatalogTaskToken {
        self.presentation_tasks.cancel();
        self.cancel_search();
        self.group_progress.clear();
        let token = self.next_catalog_task();
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.testing.clear();
        self.active_testing_groups.clear();
        self.catalog_presentation.begin_operation();
        self.restoring_auto = None;
        self.measuring_and_restoring_auto = None;
        token
    }

    pub(super) fn operation_pending(&self) -> bool {
        self.switching.any_pending()
            || self.restoring_auto.is_some()
            || self.measuring_and_restoring_auto.is_some()
    }

    pub(super) fn catalog_operation_pending(&self) -> bool {
        self.restoring_auto.is_some() || self.measuring_and_restoring_auto.is_some()
    }

    pub(super) fn proxy_selection_blocked(&self, group: &str) -> bool {
        self.catalog_operation_pending() || self.switching.group_pending(group)
    }
}

#[derive(Default)]
pub(super) struct PresentationTasks(Vec<tokio::task::AbortHandle>);

impl PresentationTasks {
    pub(super) fn track<T>(&mut self, task: &tokio::task::JoinHandle<T>) {
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

pub(super) async fn receive_batch<T, D: std::future::Future<Output = ()>>(
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
