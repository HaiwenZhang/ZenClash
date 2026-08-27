use super::{
    load_page, load_page_with_binary, AppContext, ConfigInputs, Context, ControlledConfigStore,
    Duration, HashSet, InputEvent, InputState, LiveTrafficSample, MihomoLogLevel, Page,
    PageTaskToken, ProfileActivated, ProfileCatalog, ProfileStore, RuntimeConfig,
    RuntimeConfigApplied, RuntimeData, RuntimePage, RuntimePageServices, Value, VecDeque, Window,
    YamlOverrideCatalog, YamlOverrideStore,
};

struct InitialPersistentState {
    profile_store: Option<ProfileStore>,
    profile_catalog: ProfileCatalog,
    controlled_config: Value,
    effective_config: Value,
    override_store: Option<YamlOverrideStore>,
    override_catalog: YamlOverrideCatalog,
    error: Option<String>,
}

fn load_initial_persistent_state(
    profile_path: Option<&std::path::Path>,
    controlled_store: &ControlledConfigStore,
) -> InitialPersistentState {
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
    let (controlled_config, controlled_error) = controlled_store.load_json().map_or_else(
        |error| (empty_json_object(), Some(error.to_string())),
        |value| (value, None),
    );
    let (override_store, override_catalog, override_error) = match YamlOverrideStore::discover() {
        Ok(store) => match store.load() {
            Ok(catalog) => (Some(store), catalog, None),
            Err(error) => (
                Some(store),
                YamlOverrideCatalog::default(),
                Some(error.to_string()),
            ),
        },
        Err(error) => (
            None,
            YamlOverrideCatalog::default(),
            Some(error.to_string()),
        ),
    };
    let override_paths = override_store
        .as_ref()
        .map_or_else(Vec::new, |store| store.enabled_paths(&override_catalog));
    let (effective_config, effective_error) = profile_path
        .map_or_else(
            || Ok(empty_json_object()),
            |profile| controlled_store.effective_json_with_overrides(profile, &override_paths),
        )
        .map_or_else(
            |error| (empty_json_object(), Some(error.to_string())),
            |value| (value, None),
        );
    InitialPersistentState {
        profile_store,
        profile_catalog,
        controlled_config,
        effective_config,
        override_store,
        override_catalog,
        error: store_error
            .or(controlled_error)
            .or(effective_error)
            .or(override_error),
    }
}

fn empty_json_object() -> Value {
    Value::Object(serde_json::Map::default())
}

impl RuntimePage {
    pub(crate) fn report_managed_core_state(&mut self, running: bool, cx: &mut Context<Self>) {
        const FAILURE_PREFIX: &str = "托管内核意外退出";
        if running {
            if self
                .startup_error
                .as_deref()
                .is_some_and(|error| error.starts_with(FAILURE_PREFIX))
            {
                self.startup_error = None;
                self.notice = Some("托管内核已重新启动，运行状态与系统代理正在恢复".into());
            }
        } else {
            self.startup_error = Some(format!(
                "{FAILURE_PREFIX}，ZenClash 正在释放其接管的系统代理以避免网络中断。请在“内核”页面查看日志并重启。"
            ));
        }
        cx.notify();
    }

    pub(crate) fn preferences_restored_from_app(
        &mut self,
        preferences: super::AppPreferences,
        cx: &mut Context<Self>,
    ) {
        self.preferences = preferences;
        cx.notify();
    }

    pub(crate) fn report_system_proxy_reconcile_error(
        &mut self,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        self.error = Some(format!("系统代理与新内核配置同步失败：{error}"));
        cx.notify();
    }

    pub(crate) fn profile_updated_in_background(
        &mut self,
        outcome: super::profiles::workflow::BackgroundUpdateOutcome,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.reload_profile_catalog() {
            tracing::warn!(%error, "failed to reload profile catalog after background update");
        }
        self.notice = Some(format!("在线订阅“{}”已自动更新", outcome.name));
        if outcome.active {
            self.profile_path = Some(outcome.path.clone());
            self.invalidate_config_inputs();
            self.config_preview = None;
            cx.emit(ProfileActivated { path: outcome.path });
        }
        cx.notify();
    }

    pub(crate) fn report_background_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        tracing::warn!(%error, "automatic profile update failed");
        if self.page == Page::Profiles {
            self.error = Some(format!("自动更新失败：{error}"));
            cx.notify();
        }
    }

    /// Creates the runtime page host and starts its bounded live-update loop.
    pub fn new(
        page: Page,
        services: RuntimePageServices,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RuntimePageServices {
            core_kind,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            process,
            profile_path,
            controlled_config_store,
            preferences_store,
            preferences,
            system_proxy_controller,
            traffic_history_store,
            startup_notice,
            startup_error,
        } = services;
        let InitialPersistentState {
            profile_store,
            profile_catalog,
            controlled_config,
            effective_config,
            override_store,
            override_catalog,
            error,
        } = load_initial_persistent_state(profile_path.as_deref(), &controlled_config_store);
        let (webdav, webdav_error) = super::settings::webdav::WebDavUiState::discover(window, cx);
        let config_inputs = ConfigInputs::new(&effective_config, window, cx);
        let config_inputs_profile = profile_path.clone();
        let profile_forms = super::profiles::ProfileFormState::new(window, cx);
        let profile_editor = super::overrides::ProfileEditorState::new(window, cx);
        let network_latency_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("例如：公司网关"));
        let network_latency_url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://example.com/generate_204"));
        let connection_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("过滤域名、IP、进程或规则…"));
        let connection_filter_subscription =
            cx.subscribe(&connection_filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        let log_filter = cx.new(|cx| InputState::new(window, cx).placeholder("过滤级别或内容…"));
        let log_filter_subscription = cx.subscribe(&log_filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let rule_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("过滤规则类型、内容或策略…"));
        let rule_filter_subscription =
            cx.subscribe(&rule_filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        let mut this = Self {
            page,
            core_kind,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            process,
            profile_path,
            profile_store,
            controlled_config_store,
            controlled_config,
            config_inputs,
            config_inputs_profile,
            profile_catalog,
            webdav,
            preferences_store,
            preferences,
            core_management: super::settings::CoreManagementUiState::default(),
            system_proxy_controller,
            traffic_history_store,
            profile_forms,
            network_latency_name,
            network_latency_url,
            connection_filter,
            log_filter,
            rule_filter,
            system_proxy_editor: None,
            core_releases: super::CoreReleaseState::default(),
            override_store,
            override_catalog,
            config_preview: None,
            profile_editor,
            data: RuntimeData::Empty,
            traffic_samples: VecDeque::from(vec![LiveTrafficSample::default(); 48]),
            traffic_history: super::traffic::TrafficHistoryUiState::default(),
            network_probe: super::network::NetworkProbeUiState::default(),
            ruleset: super::resources::RulesetUiState::default(),
            navigation_generation: 0,
            load_generation: 0,
            loading: false,
            mutating: false,
            closing_connections: HashSet::new(),
            error: error.or(webdav_error),
            startup_error,
            notice: startup_notice,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![
                connection_filter_subscription,
                log_filter_subscription,
                rule_filter_subscription,
            ],
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
        if self.page == Page::Network {
            self.cancel_network_probe();
        }
        self.page = page;
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        self.data = RuntimeData::Empty;
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = false;
        self.error = None;
        self.notice = None;
        self.reload_controlled_config(cx);
        if page == Page::Profiles {
            if let Err(error) = self.reload_profile_catalog() {
                self.error = Some(error);
            }
        }
        self.refresh(cx);
        if page == Page::Settings {
            self.refresh_core_management(cx);
        }
        if page == Page::Traffic {
            self.refresh_traffic_history(cx);
        }
    }

    fn start_live_updates(cx: &mut Context<Self>) {
        let mut history_ticks = 0_u8;
        let mut dashboard_ticks = 0_u8;
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if this
                .update(cx, |this, cx| {
                    let traffic = this.traffic_monitor.snapshot();
                    if this.traffic_samples.len() >= 48 {
                        this.traffic_samples.pop_front();
                    }
                    this.traffic_samples.push_back(LiveTrafficSample {
                        upload: traffic.upload,
                        download: traffic.download,
                    });
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
                    if this.page == Page::Traffic {
                        history_ticks = history_ticks.saturating_add(1);
                        if history_ticks >= 10 {
                            history_ticks = 0;
                            this.refresh_traffic_history(cx);
                        }
                    } else {
                        history_ticks = 0;
                    }
                    if this.page == Page::Home {
                        cx.notify();
                        dashboard_ticks = dashboard_ticks.saturating_add(1);
                        if dashboard_ticks >= 4 && !this.loading && !this.mutating {
                            dashboard_ticks = 0;
                            this.refresh(cx);
                        }
                    } else {
                        dashboard_ticks = 0;
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
        let mihomo_binary = self.mihomo_binary();
        let task = self
            .runtime
            .spawn(async move { load_page_with_binary(client, page, mihomo_binary).await });
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
                    Ok(data) => {
                        this.data = data;
                        if page == Page::Network {
                            this.refresh_network_probe(cx);
                        }
                    }
                    Err(error) => {
                        if this.startup_error.is_none() {
                            this.error = Some(error);
                        } else {
                            tracing::debug!(%error, "offline recovery controller probe failed");
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn apply_controlled_config(
        &mut self,
        patch: Value,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let requested_log_level = patch
            .get("log-level")
            .and_then(Value::as_str)
            .and_then(MihomoLogLevel::from_api);
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some("未配置当前配置文件路径".into());
            cx.notify();
            return;
        };
        let page = self.page;
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let overrides = self.enabled_override_paths();
        let client = self.client.clone();
        let uses_restart = !self.core_kind.capabilities().full_config_reload;
        let process = self.process.clone();
        if uses_restart && process.is_none() {
            self.mutating = false;
            self.error = Some(format!(
                "外部 {} 不支持完整配置热重载；请由 ZenClash 托管该内核后重试",
                self.core_kind.display_name()
            ));
            cx.notify();
            return;
        }
        let task = self.runtime.spawn(async move {
            if uses_restart {
                let process = process.ok_or_else(|| {
                    "托管内核在配置任务启动前已不可用，请重启 ZenClash".to_owned()
                })?;
                controlled
                    .apply_json_update_with_restart(process, profile, &patch, overrides)
                    .await
                    .map_err(|error| error.to_string())?;
            } else {
                controlled
                    .apply_json_update_with_overrides(&client, profile, &patch, overrides)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let controlled_config = controlled.load_json().map_err(|error| error.to_string())?;
            let data = load_page(client, page).await?;
            Ok::<_, String>((data, controlled_config))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("受控配置任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((data, controlled_config)) => {
                        if let Some(level) = requested_log_level {
                            this.log_monitor.set_level(level);
                        }
                        this.controlled_config = controlled_config;
                        this.config_preview = None;
                        cx.emit(RuntimeConfigApplied);
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if uses_restart {
                                format!(
                                    "{}（{} 已重启并通过控制器验收）",
                                    success.replace("热重载", "保存"),
                                    this.core_kind.display_name()
                                )
                            } else {
                                success.into()
                            });
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn controlled_bool(&self, pointer: &str, fallback: bool) -> bool {
        self.controlled_config
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    }

    pub(super) fn refresh_config_inputs_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config_inputs_profile == self.profile_path {
            return;
        }
        let Some(profile) = self.profile_path.as_ref() else {
            self.config_inputs_profile = None;
            return;
        };
        match self.controlled_config_store.effective_json(profile) {
            Ok(config) => {
                self.config_inputs = ConfigInputs::new(&config, window, cx);
                self.config_inputs_profile = Some(profile.clone());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub(super) fn invalidate_config_inputs(&mut self) {
        self.config_inputs_profile = None;
    }

    pub(crate) fn reload_controlled_config(&mut self, cx: &mut Context<Self>) {
        match self.controlled_config_store.load_json() {
            Ok(controlled) => self.controlled_config = controlled,
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(crate) fn profile_activated_from_tray(
        &mut self,
        path: std::path::PathBuf,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.profile_path = Some(path.clone());
        self.invalidate_config_inputs();
        self.config_preview = None;
        if let Err(error) = self.reload_profile_catalog() {
            self.error = Some(error);
        }
        self.notice = Some(format!("已从状态栏切换到“{name}”"));
        cx.emit(super::ProfileActivated { path });
        self.refresh(cx);
    }

    pub(crate) fn report_tray_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.error = Some(format!("状态栏切换配置失败：{error}"));
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
            RuntimeData::Dashboard { config, .. }
            | RuntimeData::Config(config)
            | RuntimeData::Core { config, .. }
            | RuntimeData::Profile { config, .. }
            | RuntimeData::Resources { config, .. }
            | RuntimeData::SystemProxy { config, .. }
            | RuntimeData::Network { config, .. }
            | RuntimeData::Tun { config, .. }
            | RuntimeData::Settings { config, .. } => Some(config),
            _ => None,
        }
    }

    pub(super) fn mihomo_binary(&self) -> Option<std::path::PathBuf> {
        self.process
            .as_ref()
            .map(|process| process.snapshot().binary)
            .or_else(|| std::env::var_os("ZENCLASH_MIHOMO_BINARY").map(std::path::PathBuf::from))
    }
}
