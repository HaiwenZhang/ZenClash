use super::{
    append_delay, take_untested_proxies, test_key, CatalogTaskToken, Context, ProxiesPage,
    ProxyGroup,
};
use futures_util::{stream, StreamExt};

const MAX_DELAY_TEST_CONCURRENCY: usize = 16;

impl ProxiesPage {
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(false, cx);
    }

    fn start_refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        if !force && (self.loading || self.switching.is_some()) {
            return;
        }
        let token = self.begin_catalog_operation();
        self.loading = true;
        self.error = None;
        cx.notify();

        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let (catalog, config) =
                tokio::try_join!(client.proxy_catalog(), client.runtime_config())
                    .map_err(|error| error.to_string())?;
            Ok::<_, String>((catalog, config.mode))
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("代理数据任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((catalog, mode)) => {
                        if this.expanded.is_empty() {
                            if let Some(group) = catalog.groups_for_mode(&mode).next() {
                                this.expanded.insert(group.name.clone());
                            }
                        }
                        this.catalog = Some(catalog);
                        this.outbound_mode = mode;
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

    /// Clears profile-specific presentation state before loading a new profile.
    pub fn profile_activated(&mut self, cx: &mut Context<Self>) {
        self.catalog = None;
        self.expanded.clear();
        self.start_refresh(true, cx);
    }

    pub(super) fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.to_owned());
        }
        cx.notify();
    }

    pub(crate) fn set_outbound_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        if self.outbound_mode.eq_ignore_ascii_case(mode) {
            return;
        }
        self.outbound_mode = mode.to_ascii_lowercase();
        self.expanded.clear();
        if let Some(catalog) = &self.catalog {
            if let Some(group) = catalog.groups_for_mode(&self.outbound_mode).next() {
                self.expanded.insert(group.name.clone());
            }
        }
        cx.notify();
    }

    pub(super) fn change_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.switching.is_some() || self.loading {
            return;
        }
        let token = self.begin_catalog_operation();
        self.switching = Some((group.clone(), proxy.clone()));
        self.error = None;
        cx.notify();

        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client.change_proxy(&group, &proxy).await?;
            client.close_all_connections().await?;
            client.proxy_catalog().await
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(format!("切换代理任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                this.switching = None;
                match result {
                    Ok(catalog) => {
                        this.catalog = Some(catalog);
                        this.error = None;
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
        let test_key = test_key(&group, &proxy);
        if !self.testing.insert(test_key.clone()) {
            return;
        }
        let token = CatalogTaskToken(self.catalog_generation);
        cx.notify();

        let client = self.client.clone();
        let proxy_for_task = proxy.clone();
        let task = self.runtime.spawn(async move {
            client
                .proxy_delay_with_provider(
                    &proxy_for_task,
                    test_url.as_deref(),
                    5_000,
                    provider.as_deref(),
                )
                .await
                .map_err(|error| error.to_string())
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("延迟测试任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                this.testing.remove(&test_key);
                match result {
                    Ok(result) => {
                        this.record_delay(&group, &proxy, result.delay, result.mean_delay);
                    }
                    Err(error) => {
                        this.record_delay(&group, &proxy, 0, 0);
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn test_group(&mut self, group: &ProxyGroup, cx: &mut Context<Self>) {
        let proxies = take_untested_proxies(&mut self.testing, group);
        if proxies.is_empty() {
            return;
        }
        let pending = proxies
            .iter()
            .map(|proxy| test_key(&group.name, &proxy.name))
            .collect::<Vec<_>>();
        let token = CatalogTaskToken(self.catalog_generation);
        self.error = None;
        cx.notify();

        let client = self.client.clone();
        let group_name = group.name.clone();
        let test_url = group.test_url.clone();
        let task = self.runtime.spawn(async move {
            stream::iter(proxies.into_iter().map(|proxy| {
                let client = client.clone();
                let test_url = test_url.clone();
                async move {
                    let result = client
                        .proxy_delay_with_provider(
                            &proxy.name,
                            test_url.as_deref(),
                            5_000,
                            proxy.provider_name.as_deref(),
                        )
                        .await;
                    (proxy.name, result)
                }
            }))
            .buffer_unordered(MAX_DELAY_TEST_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
        });

        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("代理组延迟测试任务异常结束：{error}"));
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    return;
                }
                for key in pending {
                    this.testing.remove(&key);
                }
                match result {
                    Ok(results) => {
                        let mut failed = 0usize;
                        let mut first_error = None;
                        for (proxy, result) in results {
                            if let Ok(result) = result {
                                this.record_delay(
                                    &group_name,
                                    &proxy,
                                    result.delay,
                                    result.mean_delay,
                                );
                            } else if let Err(error) = result {
                                failed += 1;
                                first_error.get_or_insert_with(|| error.to_string());
                                this.record_delay(&group_name, &proxy, 0, 0);
                            }
                        }
                        if failed > 0 {
                            this.error = Some(match first_error {
                                Some(error) => {
                                    format!("{failed} 个节点延迟测试失败；首个错误：{error}")
                                }
                                None => format!("{failed} 个节点延迟测试失败"),
                            });
                        }
                    }
                    Err(error) => this.error = Some(error),
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
    }

    fn begin_catalog_operation(&mut self) -> CatalogTaskToken {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.testing.clear();
        self.switching = None;
        CatalogTaskToken(self.catalog_generation)
    }
}
