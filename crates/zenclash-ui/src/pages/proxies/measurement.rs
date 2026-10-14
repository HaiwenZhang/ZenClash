//! Catalog-bound single and batch measurements and automatic-selection readback.
use super::actions::{MAX_DELAY_TEST_CONCURRENCY, receive_batch};
use super::{
    Context, DelayTaskToken, ProxiesPage, ProxyDelayTarget, ProxyNodeId, ProxyTestKey,
    insert_inflight_test, remove_inflight_test, take_untested_group_proxies,
};
use futures_util::{StreamExt, stream};

impl ProxiesPage {
    pub(super) fn restore_auto(&mut self, group: String, cx: &mut Context<Self>) {
        if self.operation_pending() || self.loading {
            return;
        }
        let Some(operations) = self.capture_catalog_mutation_source(cx) else {
            return;
        };
        let token = self.begin_catalog_operation();
        self.restoring_auto = Some(group.clone());
        self.error = None;
        self.notice = None;
        cx.notify();

        let operation_runtime = operations.clone();
        let session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            match session {
                Some(session) => {
                    let identity = operations
                        .observation_identity()
                        .ok_or_else(|| zenclash_core::MihomoError::StaleBinding.to_string())?;
                    session
                        .restore_observed_proxy_group(identity, &group)
                        .await
                        .map(zenclash_core::ProxyGroupActionReceipt::into_parts)
                        .map_err(|error| error.to_string())
                }
                None => {
                    let outcome = operations
                        .restore_auto(&group)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok((operations, outcome))
                }
            }
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
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
                if let Ok((_, outcome)) = &result {
                    for warning in &outcome.warnings {
                        tracing::warn!(%warning, "automatic proxy group restored with a warning");
                    }
                }
                let publication_runtime = result.as_ref().map_or_else(
                    |_| operation_runtime.clone(),
                    |completed| completed.0.clone(),
                );
                let published = publication_runtime.publish_if_current(|| match result {
                    Ok((_, outcome)) => {
                        let warning = (!outcome.warnings.is_empty()).then(|| {
                            zenclash_i18n::text_with(
                                "proxies.notices.restored_with_warning",
                                &[("warning", outcome.warnings.join("; "))],
                            )
                        });
                        if let Some(catalog) = outcome.catalog {
                            let indices = super::presentation::visible_group_indices(
                                &catalog,
                                &this.outbound_mode,
                                false,
                            );
                            this.install_catalog(catalog, this.outbound_mode.clone(), indices);
                            this.catalog_operations = Some(publication_runtime.clone());
                        }
                        this.error = None;
                        this.notice = warning
                            .or_else(|| Some(zenclash_i18n::text("proxies.notices.restored_auto")));
                    }
                    Err(error) => this.error = Some(error),
                });
                if let Err(error) = published {
                    this.error = Some(error.to_string());
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
        let Some(operations) = self.capture_catalog_mutation_source(cx) else {
            return;
        };
        let token = self.begin_catalog_operation();
        self.measuring_and_restoring_auto = Some(group.clone());
        self.error = None;
        self.notice = None;
        cx.notify();

        let operation_runtime = operations.clone();
        let session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            let (operations, outcome) = match session {
                Some(session) => {
                    let identity = operations
                        .observation_identity()
                        .ok_or_else(|| zenclash_core::MihomoError::StaleBinding.to_string())?;
                    session
                        .measure_observed_proxy_group(identity, &group, test_url.as_deref(), 5_000)
                        .await
                        .map(zenclash_core::ProxyGroupActionReceipt::into_parts)
                        .map_err(|error| error.to_string())?
                }
                None => {
                    let outcome = operations
                        .measure_group_and_restore_auto(&group, test_url.as_deref(), 5_000)
                        .await
                        .map_err(|error| error.to_string())?;
                    (operations, outcome)
                }
            };
            Ok::<_, String>((operations, group, outcome))
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
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
                if let Ok((_, _, outcome)) = &result {
                    for warning in &outcome.selection.warnings {
                        tracing::warn!(%warning, "group delay completed with a readback warning");
                    }
                }
                let publication_runtime = result.as_ref().map_or_else(
                    |_| operation_runtime.clone(),
                    |completed| completed.0.clone(),
                );
                let published = publication_runtime.publish_if_current(|| match result {
                    Ok((_, group, outcome)) => {
                        let warning = (!outcome.selection.warnings.is_empty()).then(|| {
                            zenclash_i18n::text_with(
                                "proxies.notices.restored_with_warning",
                                &[("warning", outcome.selection.warnings.join("; "))],
                            )
                        });
                        if let Some(catalog) = outcome.selection.catalog {
                            let indices = super::presentation::visible_group_indices(
                                &catalog,
                                &this.outbound_mode,
                                false,
                            );
                            this.install_catalog(catalog, this.outbound_mode.clone(), indices);
                            this.catalog_operations = Some(publication_runtime.clone());
                        }
                        let results = this
                            .catalog
                            .as_ref()
                            .and_then(|catalog| {
                                let group = catalog
                                    .groups()
                                    .iter()
                                    .find(|item| item.controller_name() == group)?;
                                Some(
                                    group
                                        .all
                                        .iter()
                                        .filter_map(|id| {
                                            outcome
                                                .delays
                                                .get(id.controller_name())
                                                .copied()
                                                .map(|delay| (id.clone(), delay))
                                        })
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .unwrap_or_default();
                        for (id, delay) in results {
                            this.record_node_delay(&id, delay, delay, None);
                        }
                        this.error = None;
                        this.notice = warning.or_else(|| {
                            Some(zenclash_i18n::text(
                                "proxies.notices.measured_and_restored_auto",
                            ))
                        });
                    }
                    Err(error) => this.error = Some(error),
                });
                if let Err(error) = published {
                    this.error = Some(error.to_string());
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
        let Some(operations) = self.capture_catalog_operations(cx) else {
            return;
        };
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

        let operation_runtime = operations.clone();
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
                this.publish_delay_result(token, &operation_runtime, |this| match result {
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
                });
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
        let Some(operations) = self.capture_catalog_operations(cx) else {
            return;
        };
        let Some(catalog) = self.catalog.as_deref() else {
            return;
        };
        let Some(group) = catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == group_name)
        else {
            return;
        };
        let group_name = group.controller_name().to_owned();
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

        let operation_runtime = operations.clone();
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
                    let published = this.publish_delay_result(token, &operation_runtime, |this| {
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
                                Ok(result) => this.record_node_delay(
                                    &node,
                                    result.delay,
                                    result.mean_delay,
                                    None,
                                ),
                                Err(error) => {
                                    failures += 1;
                                    this.record_node_delay(
                                        &node,
                                        0,
                                        0,
                                        Some(super::DelayTestFailure::from_error(&error)),
                                    );
                                }
                            }
                        }
                    });
                    if !published {
                        this.testing.remove(&group_name);
                        this.active_testing_groups.remove(&group_name);
                        this.group_progress.remove(&group_name);
                        cx.notify();
                        return false;
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
            let error = match task.await {
                Err(error) => Some(zenclash_i18n::text_with(
                    "proxies.errors.group_delay_task",
                    &[("error", error.to_string())],
                )),
                Ok(()) if failures > 0 => Some(zenclash_i18n::text_with(
                    "proxies.errors.group_failed",
                    &[
                        ("group", group_name.clone()),
                        ("count", failures.to_string()),
                    ],
                )),
                Ok(()) => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_group_measurement(token, &operation_runtime, &group_name, error, cx);
            });
        })
        .detach();
    }
    pub(super) fn finish_group_measurement(
        &mut self,
        token: DelayTaskToken,
        operations: &super::ProxyOperations,
        group: &str,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !token.is_current(self.delay_generation) {
            return;
        }
        self.group_progress.remove(group);
        self.publish_delay_result(token, operations, |this| {
            if let Some(error) = error {
                this.error = Some(error);
            }
        });
        if !self.search_query.is_empty() {
            self.prepare_search(cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
