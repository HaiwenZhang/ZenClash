use super::{
    load_page, AppContext, Context, Duration, HashSet, InputState, Page, PageTaskToken,
    ProfileCatalog, ProfileStore, RuntimeConfig, RuntimeData, RuntimePage, RuntimePageServices,
    Value, VecDeque, Window,
};

impl RuntimePage {
    /// Creates the runtime page host and starts its bounded live-update loop.
    pub fn new(
        page: Page,
        services: RuntimePageServices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RuntimePageServices {
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            process,
            profile_path,
        } = services;
        let (profile_store, profile_catalog, store_error) = match ProfileStore::discover() {
            Ok(store) => match store.load() {
                Ok(catalog) => (Some(store), catalog, None),
                Err(error) => (
                    Some(store),
                    ProfileCatalog::default(),
                    Some(error.to_string()),
                ),
            },
            Err(error) => (None, ProfileCatalog::default(), Some(error.to_string())),
        };
        let subscription_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("例如：机场主订阅"));
        let subscription_url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://example.com/api/v1/client/subscribe…")
        });
        let subscription_user_agent = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("clash.meta")
                .placeholder("clash.meta")
        });
        let mut this = Self {
            page,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            process,
            profile_path,
            profile_store,
            profile_catalog,
            subscription_name,
            subscription_url,
            subscription_user_agent,
            override_paths: Vec::new(),
            data: RuntimeData::Empty,
            traffic_samples: VecDeque::from(vec![0; 48]),
            navigation_generation: 0,
            load_generation: 0,
            loading: false,
            mutating: false,
            closing_connections: HashSet::new(),
            error: store_error,
            notice: None,
            focus_handle: cx.focus_handle(),
        };
        this.refresh(cx);
        Self::start_live_updates(cx);
        this
    }

    /// Switches the active tab and invalidates results from older page tasks.
    pub fn switch_to(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        self.data = RuntimeData::Empty;
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = false;
        self.error = None;
        self.notice = None;
        if page == Page::Profiles {
            if let Err(error) = self.reload_profile_catalog() {
                self.error = Some(error);
            }
        }
        self.refresh(cx);
    }

    fn start_live_updates(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if this
                .update(cx, |this, cx| {
                    let traffic = this.traffic_monitor.snapshot();
                    if this.traffic_samples.len() >= 48 {
                        this.traffic_samples.pop_front();
                    }
                    this.traffic_samples
                        .push_back(traffic.upload.saturating_add(traffic.download));
                    if matches!(this.page, Page::Logs) {
                        cx.notify();
                    }
                    if matches!(this.page, Page::Connections | Page::Traffic)
                        && !this.loading
                        && !this.mutating
                        && this.closing_connections.is_empty()
                    {
                        this.refresh(cx);
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.error = None;
        let page = self.page;
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(async move { load_page(client, page).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("页面数据任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                this.loading = false;
                if this.page != page {
                    return;
                }
                match result {
                    Ok(data) => this.data = data,
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn patch_config(
        &mut self,
        body: Value,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let page = self.page;
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .patch_configs(&body)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, page).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("配置更新任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                if !this.is_page_task_current(token) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(data) => {
                        this.data = data;
                        this.notice = Some(success.into());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn begin_mutation(&mut self, page: Page) -> Option<PageTaskToken> {
        if self.mutating {
            return None;
        }
        self.mutating = true;
        self.invalidate_page_load();
        self.error = None;
        self.notice = None;
        Some(self.page_task_token_for(page))
    }

    pub(super) fn invalidate_page_load(&mut self) {
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = false;
    }

    pub(super) const fn page_task_token_for(&self, page: Page) -> PageTaskToken {
        PageTaskToken {
            page,
            navigation_generation: self.navigation_generation,
        }
    }

    pub(super) fn is_page_task_current(&self, token: PageTaskToken) -> bool {
        token.is_current(self.page, self.navigation_generation)
    }

    pub(super) fn replace_page_data(&mut self, token: PageTaskToken, data: RuntimeData) -> bool {
        if !self.is_page_task_current(token) {
            return false;
        }
        self.data = data;
        true
    }

    pub(super) fn set_page_error(&mut self, token: PageTaskToken, error: String) {
        if self.is_page_task_current(token) {
            self.error = Some(error);
        } else {
            tracing::warn!(page = ?token.page, %error, "discarded error from an inactive page task");
        }
    }

    pub(super) const fn config(&self) -> Option<&RuntimeConfig> {
        match &self.data {
            RuntimeData::Config(config)
            | RuntimeData::Core { config, .. }
            | RuntimeData::Profile { config, .. }
            | RuntimeData::SystemProxy { config, .. }
            | RuntimeData::Network { config, .. } => Some(config),
            _ => None,
        }
    }
}
