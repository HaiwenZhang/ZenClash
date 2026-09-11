use super::{
    CatalogTaskToken, ConnectionPolicy, Context, DelayTaskToken, ProxiesPage, ProxyDelayTarget,
    ProxyOperations, ProxySelectionTaskToken, ProxyVisibility, append_delay,
    apply_optimistic_selection, take_untested_proxies, test_key,
};
use futures_util::{StreamExt, stream};

const MAX_DELAY_TEST_CONCURRENCY: usize = 16;

impl ProxiesPage {
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
        let visibility = if self.show_hidden {
            ProxyVisibility::IncludeHidden
        } else {
            ProxyVisibility::VisibleOnly
        };
        let task = self.runtime.spawn(async move {
            let operations = ProxyOperations::new(client.clone());
            let (catalog, config) =
                tokio::try_join!(operations.catalog(visibility), client.runtime_config())
                    .map_err(|error| error.to_string())?;
            Ok::<_, String>((catalog, config.mode))
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
                    Ok((catalog, mode)) => {
                        if this.expanded.is_empty()
                            && let Some(group) = catalog.groups_for_mode(&mode).next()
                        {
                            this.expanded.insert(group.name.clone());
                        }
                        this.group_orders.clear();
                        this.catalog = Some(catalog);
                        this.outbound_mode = mode;
                        this.test_failures.clear();
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Invalidates an older catalog request and loads current controller state.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(true, cx);
    }

    /// Invalidates in-flight presentation work and releases the inactive catalog.
    pub(crate) fn suspend(&mut self) {
        self.presentation_tasks.cancel();
        self.group_progress.clear();
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.switching.clear();
        self.group_orders.clear();
        self.catalog = None;
        self.expanded.clear();
        self.proxy_pages.clear();
        self.testing.clear();
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
        self.show_hidden = false;
    }

    pub(super) fn set_show_hidden(&mut self, show_hidden: bool, cx: &mut Context<Self>) {
        if self.show_hidden == show_hidden || self.loading || self.operation_pending() {
            return;
        }
        self.show_hidden = show_hidden;
        self.group_orders.clear();
        self.expanded.clear();
        self.proxy_pages.clear();
        self.start_refresh(true, cx);
    }

    pub(super) fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        super::toggle_expanded_group(&mut self.expanded, name);
        if !self.expanded.contains(name) {
            self.group_orders.invalidate(name);
        }
        cx.notify();
    }

    pub(super) fn set_group_page(&mut self, name: String, page: usize, cx: &mut Context<Self>) {
        self.proxy_pages.insert(name, page);
        cx.notify();
    }

    pub(crate) fn set_outbound_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        if self.outbound_mode.eq_ignore_ascii_case(mode) {
            return;
        }
        self.outbound_mode = mode.to_ascii_lowercase();
        self.group_orders.clear();
        self.expanded.clear();
        self.proxy_pages.clear();
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
                            this.group_orders.clear();
                            this.catalog = Some(catalog);
                        }
                        this.error = None;
                        this.notice = warning.or_else(|| {
                            Some(zenclash_i18n::text("proxies.notices.restored_auto"))
                        });
                    }
                    Err(error) => this.error = Some(error),
                }
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
                            this.group_orders.clear();
                            this.catalog = Some(catalog);
                        }
                        this.group_orders.invalidate(&group);
                        if let Some(group) = this.catalog.as_mut().and_then(|catalog| catalog.groups.iter_mut().find(|item| item.name == group)) {
                            for proxy in &mut group.all {
                                if let Some(&delay) = outcome.delays.get(&proxy.name) {
                                    append_delay(proxy, delay, delay);
                                }
                            }
                        }
                        this.error = None;
                        this.notice = warning.or_else(|| {
                            Some(zenclash_i18n::text(
                                "proxies.notices.measured_and_restored_auto",
                            ))
                        });
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn test_proxy(
        &mut self,
        group: String,
        proxy: String,
        test_url: Option<String>,
        provider: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.proxy_selection_blocked(&group) || self.loading {
            return;
        }
        let test_key = test_key(&group, &proxy);
        if !self.testing.insert(test_key.clone()) {
            return;
        }
        let token = DelayTaskToken(self.delay_generation);
        cx.notify();

        let operations = ProxyOperations::new(self.client.clone());
        let target = ProxyDelayTarget {
            name: proxy.clone(),
            provider,
        };
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
                if !token.is_current(this.delay_generation) {
                    return;
                }
                this.testing.remove(&test_key);
                match result {
                    Ok(result) => {
                        this.test_failures.remove(&test_key);
                        this.record_delay(&group, &proxy, result.delay, result.mean_delay);
                    }
                    Err(error) => {
                        this.test_failures.insert(
                            test_key.clone(),
                            super::DelayTestFailure::from_error(&error),
                        );
                        this.record_delay(&group, &proxy, 0, 0);
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn test_group(&mut self, group_name: &str, cx: &mut Context<Self>) {
        if self.proxy_selection_blocked(group_name)
            || self.loading
            || self.group_progress.contains_key(group_name)
        {
            return;
        }
        let Some(group) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.groups.iter().find(|group| group.name == group_name))
        else {
            return;
        };
        let group_name = group.name.clone();
        let test_url = group.test_url.clone();
        let proxies = take_untested_proxies(&mut self.testing, group);
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
            let mut measurements = stream::iter(proxies.into_iter().map(|(index, target)| {
                let operations = operations.clone();
                let test_url = test_url.clone();
                async move {
                    let result = operations
                        .measure(&target, test_url.as_deref(), 5_000)
                        .await
                        .map_err(|error| error.to_string());
                    (index, target.name, result)
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
            while let Some(batch) = receive_batch(&mut receiver).await {
                let applied = this.update(cx, |this, cx| {
                    if !token.is_current(this.delay_generation) {
                        return false;
                    }
                    let mut group = this.catalog.as_mut().and_then(|catalog| {
                        catalog
                            .groups
                            .iter_mut()
                            .find(|group| group.name == group_name)
                    });
                    this.group_orders.invalidate(&group_name);
                    for (index, name, result) in batch {
                        let key = test_key(&group_name, &name);
                        this.testing.remove(&key);
                        if let Some(progress) = this.group_progress.get_mut(&group_name) {
                            progress.0 += 1;
                        }
                        let (delay, mean_delay) = match result {
                            Ok(result) => {
                                this.test_failures.remove(&key);
                                (result.delay, result.mean_delay)
                            }
                            Err(error) => {
                                failures += 1;
                                this.test_failures
                                    .insert(key, super::DelayTestFailure::from_error(&error));
                                first_error.get_or_insert(error);
                                (0, 0)
                            }
                        };
                        if let Some(proxy) = group
                            .as_deref_mut()
                            .and_then(|group| group.all.get_mut(index))
                            .filter(|proxy| proxy.name == name)
                        {
                            append_delay(proxy, delay, mean_delay);
                        }
                    }
                    if let Some(error) = &first_error {
                        this.error = Some(zenclash_i18n::text_with(
                            "proxies.errors.group_failed_detail",
                            &[("count", failures.to_string()), ("error", error.clone())],
                        ));
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
                if let Err(error) = result {
                    this.error = Some(error.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn record_delay(&mut self, group: &str, proxy: &str, delay: u32, mean_delay: u32) {
        let Some(catalog) = self.catalog.as_mut() else {
            return;
        };
        let Some(group) = catalog.groups.iter_mut().find(|item| item.name == group) else {
            return;
        };
        let Some(proxy) = group.all.iter_mut().find(|item| item.name == proxy) else {
            return;
        };
        append_delay(proxy, delay, mean_delay);
        self.group_orders.invalidate(&group.name);
    }

    fn next_catalog_task(&mut self) -> CatalogTaskToken {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        CatalogTaskToken(self.catalog_generation)
    }

    fn begin_catalog_operation(&mut self) -> CatalogTaskToken {
        self.presentation_tasks.cancel();
        self.group_progress.clear();
        let token = self.next_catalog_task();
        self.delay_generation = self.delay_generation.wrapping_add(1);
        self.testing.clear();
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

async fn receive_batch<T>(receiver: &mut tokio::sync::mpsc::Receiver<T>) -> Option<Vec<T>> {
    let first = receiver.recv().await?;
    gpui::Timer::after(std::time::Duration::from_millis(50)).await;
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
    fn batch_wait_can_start_on_the_gpui_executor_without_a_tokio_runtime() {
        use futures_util::FutureExt;

        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        sender.try_send(42).unwrap();
        let _ = receive_batch(&mut receiver).now_or_never();
    }

    #[tokio::test]
    async fn batches_bound_ui_work_without_losing_results() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
        for index in 0..100 {
            sender.send(index).await.unwrap();
        }
        drop(sender);
        assert_eq!(
            receive_batch(&mut receiver).await.unwrap(),
            (0..64).collect::<Vec<_>>()
        );
        assert_eq!(
            receive_batch(&mut receiver).await.unwrap(),
            (64..100).collect::<Vec<_>>()
        );
        assert!(receive_batch(&mut receiver).await.is_none());
    }

    #[tokio::test]
    async fn measurement_results_are_published_before_the_producer_finishes() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
        sender.send(42).await.unwrap();
        let batch = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            receive_batch(&mut receiver),
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
