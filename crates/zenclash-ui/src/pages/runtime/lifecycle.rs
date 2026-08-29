use super::{
    AppContext, ConfigInputs, ConfigInputsTaskToken, Context, ControlledConfigStore, Duration,
    MihomoLogLevel, Page, PageTaskToken, ProfileActivated, ProfileCatalog, ProfileStore,
    RuntimeConfig, RuntimeConfigApplied, RuntimeData, RuntimePage, RuntimePageServices, Value,
    Window, YamlOverrideCatalog, YamlOverrideStore, config_input_snapshot, load_page,
    load_page_with_binary,
};
use zenclash_core::{CoreApplyKind, EffectiveConfigIntent};

const LIVE_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UiVisibility {
    window_active: bool,
    window_visible: bool,
    page_presented: bool,
}

impl UiVisibility {
    const fn new(window_active: bool) -> Self {
        Self {
            window_active,
            window_visible: true,
            page_presented: true,
        }
    }

    const fn updates_enabled(self) -> bool {
        self.window_active && self.window_visible && self.page_presented
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LiveUpdateActions {
    notify: bool,
    refresh_page: bool,
    refresh_history: bool,
}

#[derive(Debug)]
struct LiveUpdateSchedule {
    history_ticks: u8,
    dashboard_ticks: u8,
    connection_ticks: u8,
    log_revision: u64,
    traffic_revision: u64,
}

impl LiveUpdateSchedule {
    const fn new(log_revision: u64, traffic_revision: u64) -> Self {
        Self {
            history_ticks: 0,
            dashboard_ticks: 0,
            connection_ticks: 0,
            log_revision,
            traffic_revision,
        }
    }

    fn synchronize(&mut self, log_revision: u64, traffic_revision: u64) {
        self.history_ticks = 0;
        self.dashboard_ticks = 0;
        self.connection_ticks = 0;
        self.log_revision = log_revision;
        self.traffic_revision = traffic_revision;
    }

    fn tick(
        &mut self,
        page: Page,
        log_revision: u64,
        traffic_revision: u64,
        page_refresh_ready: bool,
        connections_idle: bool,
    ) -> LiveUpdateActions {
        let mut actions = LiveUpdateActions::default();
        if page == Page::Logs && log_revision != self.log_revision {
            actions.notify = true;
        }
        self.log_revision = log_revision;

        if matches!(page, Page::Home | Page::Traffic) && traffic_revision != self.traffic_revision {
            actions.notify = true;
        }
        self.traffic_revision = traffic_revision;

        if matches!(page, Page::Connections | Page::Traffic) {
            self.connection_ticks = self.connection_ticks.saturating_add(1);
            if self.connection_ticks >= 2 && page_refresh_ready && connections_idle {
                self.connection_ticks = 0;
                actions.refresh_page = true;
            }
        } else {
            self.connection_ticks = 0;
        }

        if page == Page::Traffic {
            self.history_ticks = self.history_ticks.saturating_add(1);
            if self.history_ticks >= 10 {
                self.history_ticks = 0;
                actions.refresh_history = true;
            }
        } else {
            self.history_ticks = 0;
        }

        if page == Page::Home {
            self.dashboard_ticks = self.dashboard_ticks.saturating_add(1);
            if self.dashboard_ticks >= 10 && page_refresh_ready {
                self.dashboard_ticks = 0;
                actions.refresh_page = true;
            }
        } else {
            self.dashboard_ticks = 0;
        }

        actions
    }
}

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
    pub(crate) fn preferences_restored_from_app(
        &mut self,
        preferences: super::AppPreferences,
        cx: &mut Context<Self>,
    ) {
        self.preferences = preferences;
        cx.notify();
    }

    /// Refreshes placeholders stored inside long-lived input entities after a
    /// process-wide locale change. Regular page labels update on the next render,
    /// while `InputState` requires an explicit update.
    pub(crate) fn refresh_localized_placeholders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let localized_inputs = [
            (
                self.network_probe.latency_name.clone(),
                "runtime.placeholders.network_target",
            ),
            (
                self.network_probe.dns_name.clone(),
                "runtime.placeholders.dns_name",
            ),
            (
                self.connections.filter.clone(),
                "runtime.placeholders.connection_filter",
            ),
            (self.logs.filter.clone(), "runtime.placeholders.log_filter"),
            (
                self.rules.filter.clone(),
                "runtime.placeholders.rule_filter",
            ),
            (
                self.config_inputs.core.mixed_port.clone(),
                "config_inputs.placeholders.mixed_port",
            ),
            (
                self.config_inputs.core.interface_name.clone(),
                "config_inputs.placeholders.automatic_interface",
            ),
            (
                self.config_inputs.dns.fake_ip_filter.clone(),
                "config_inputs.placeholders.one_domain_or_rule",
            ),
            (
                self.config_inputs.dns.default_nameserver.clone(),
                "config_inputs.placeholders.one_ip_dns",
            ),
            (
                self.config_inputs.dns.nameserver.clone(),
                "config_inputs.placeholders.one_dns",
            ),
            (
                self.config_inputs.dns.proxy_server_nameserver.clone(),
                "config_inputs.placeholders.proxy_resolver",
            ),
            (
                self.config_inputs.dns.direct_nameserver.clone(),
                "config_inputs.placeholders.direct_resolver",
            ),
            (
                self.config_inputs.dns.fallback_ipcidr.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
            (
                self.config_inputs.dns.fallback_domain.clone(),
                "config_inputs.placeholders.one_domain_rule",
            ),
            (
                self.config_inputs.dns.nameserver_policy.clone(),
                "config_inputs.placeholders.dns_mapping",
            ),
            (
                self.config_inputs.dns.hosts.clone(),
                "config_inputs.placeholders.address_mapping",
            ),
            (
                self.config_inputs.sniffer.skip_domain.clone(),
                "config_inputs.placeholders.one_domain",
            ),
            (
                self.config_inputs.sniffer.force_domain.clone(),
                "config_inputs.placeholders.one_domain",
            ),
            (
                self.config_inputs.sniffer.skip_dst_address.clone(),
                "config_inputs.placeholders.one_address",
            ),
            (
                self.config_inputs.sniffer.skip_src_address.clone(),
                "config_inputs.placeholders.one_address",
            ),
            (
                self.config_inputs.tun.device.clone(),
                "config_inputs.placeholders.tun_device",
            ),
            (
                self.config_inputs.tun.mtu.clone(),
                "config_inputs.placeholders.default_mtu",
            ),
            (
                self.config_inputs.tun.route_include_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
            (
                self.config_inputs.tun.route_exclude_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
        ];
        for (input, key) in localized_inputs {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
        self.profile_forms
            .refresh_localized_placeholders(window, cx);
        self.profile_editor
            .refresh_localized_placeholder(window, cx);
        cx.notify();
    }

    pub(crate) fn report_system_proxy_reconcile_error(
        &mut self,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        self.error = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.proxy_reconcile",
            &[("error", error.to_owned())],
        ));
        cx.notify();
    }

    pub(crate) fn profile_updated_in_background(
        &mut self,
        outcome: super::profiles::workflow::BackgroundUpdateOutcome,
        cx: &mut Context<Self>,
    ) {
        self.reload_profile_catalog(cx);
        self.notice = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.profile_updated",
            &[("name", outcome.name)],
        ));
        if outcome.active {
            self.profile_path = Some(outcome.path.clone());
            self.invalidate_config_inputs(cx);
            self.config_preview = None;
            cx.emit(ProfileActivated { path: outcome.path });
        }
        cx.notify();
    }

    pub(crate) fn report_background_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        tracing::warn!(%error, "automatic profile update failed");
        if self.page == Page::Profiles {
            self.error = Some(zenclash_i18n::text_with(
                "runtime.lifecycle.profile_update_failed",
                &[("error", error.to_owned())],
            ));
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
            core_session,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            operational_status,
            traffic_capture,
            process,
            profile_path,
            controlled_config_store,
            preferences_store,
            preferences,
            system_proxy_session,
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
        let effective_config = config_input_snapshot(effective_config);
        let config_inputs = ConfigInputs::new(&effective_config, window, cx);
        let config_inputs_profile = profile_path.clone();
        let profile_forms = super::profiles::ProfileFormState::new(window, cx);
        let profile_editor = super::overrides::ProfileEditorState::new(window, cx);
        let provider_operations = super::ProviderOperations::new(client.clone());
        let network_probe = super::network::NetworkProbeUiState::new(window, cx);
        let (connections, connection_filter_subscription) =
            super::connections::ConnectionsUiState::new(window, cx);
        let (logs, log_filter_subscription) = super::logs::LogUiState::new(window, cx);
        let (rules, rule_filter_subscription) = super::rules::RulesUiState::new(window, cx);
        let ui_visibility = UiVisibility::new(window.is_window_active());
        let (live_updates_enabled, live_update_activity) =
            tokio::sync::watch::channel(ui_visibility.updates_enabled());
        let mut this = Self {
            page,
            core_kind,
            core_session,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            operational_status,
            traffic_capture,
            process,
            profile_path,
            profile_store,
            controlled_config_store,
            controlled_config,
            config_inputs,
            config_inputs_profile,
            config_inputs_generation: 0,
            config_inputs_loading: false,
            profile_catalog,
            profile_catalog_generation: 0,
            preferences_store,
            preferences,
            core_management: super::settings::CoreManagementUiState::default(),
            app_update: super::settings::AppUpdateUiState::default(),
            system_proxy_session,
            traffic_history_store,
            profile_forms,
            connections,
            logs,
            rules,
            system_proxy_editor: None,
            core_releases: super::CoreReleaseState::default(),
            override_store,
            override_catalog,
            config_preview: None,
            profile_editor,
            data: RuntimeData::Empty,
            home: super::home::HomeUiState::default(),
            traffic_history: super::traffic::TrafficHistoryUiState::default(),
            network_probe,
            provider_operations,
            ruleset: super::resources::RulesetUiState::default(),
            navigation_generation: 0,
            load_generation: 0,
            controlled_config_generation: 0,
            loading: false,
            mutating: false,
            error,
            startup_error,
            notice: startup_notice,
            focus_handle: cx.focus_handle(),
            window_handle: window.window_handle(),
            ui_visibility,
            live_updates_enabled,
            _subscriptions: vec![
                connection_filter_subscription,
                log_filter_subscription,
                rule_filter_subscription,
            ],
        };
        let window_activation_subscription =
            cx.observe_window_activation(window, |this, window, cx| {
                this.set_window_active(window.is_window_active(), cx);
            });
        this._subscriptions.push(window_activation_subscription);
        this.refresh(cx);
        this.refresh_app_update(cx);
        Self::start_operational_updates(
            this.operational_status.subscribe(),
            this.live_updates_enabled.subscribe(),
            cx,
        );
        Self::start_live_updates(
            live_update_activity,
            this.log_monitor.revision(),
            this.traffic_monitor.revision(),
            cx,
        );
        this
    }

    fn start_operational_updates(
        mut updates: zenclash_core::OperationalStatusStream,
        activity: tokio::sync::watch::Receiver<bool>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                if !*activity.borrow() {
                    continue;
                }
                if this
                    .update(cx, |this, cx| {
                        if this.live_updates_enabled() && this.page == Page::Home {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Switches the active tab and invalidates results from older page tasks.
    pub fn switch_to(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        let previous_page = self.page;
        if self.page == Page::Network {
            self.cancel_network_probe();
        }
        if previous_page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if previous_page == Page::Override && !self.profile_editor.is_open() {
            self.config_preview = None;
        }
        self.page = page;
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        self.data = RuntimeData::Empty;
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = false;
        self.error = None;
        self.notice = None;
        if self.live_updates_enabled() {
            self.refresh_visible_page(cx);
        }
    }

    pub(crate) fn set_presented(&mut self, presented: bool, cx: &mut Context<Self>) {
        if self.ui_visibility.page_presented == presented {
            return;
        }
        if !presented && !self.mutating {
            self.release_inactive_page_data();
        }
        let was_enabled = self.ui_visibility.updates_enabled();
        self.ui_visibility.page_presented = presented;
        self.update_live_update_activity(was_enabled, cx);
    }

    pub(crate) fn set_window_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.ui_visibility.window_visible == visible {
            return;
        }
        if !visible && !self.mutating {
            self.release_inactive_page_data();
        }
        let was_enabled = self.ui_visibility.updates_enabled();
        self.ui_visibility.window_visible = visible;
        self.update_live_update_activity(was_enabled, cx);
    }

    fn release_inactive_page_data(&mut self) {
        if self.page == Page::Network {
            self.cancel_network_probe();
        }
        if self.page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if self.page == Page::Override && !self.profile_editor.is_open() {
            self.config_preview = None;
        }
        self.invalidate_page_load();
        self.data = RuntimeData::Empty;
    }

    fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.ui_visibility.window_active == active {
            return;
        }
        let was_enabled = self.ui_visibility.updates_enabled();
        self.ui_visibility.window_active = active;
        self.update_live_update_activity(was_enabled, cx);
    }

    fn update_live_update_activity(&mut self, was_enabled: bool, cx: &mut Context<Self>) {
        let enabled = self.ui_visibility.updates_enabled();
        if was_enabled == enabled {
            return;
        }
        self.live_updates_enabled.send_replace(enabled);
        if enabled {
            self.refresh_visible_page(cx);
            cx.notify();
        }
    }

    const fn live_updates_enabled(&self) -> bool {
        self.ui_visibility.updates_enabled()
    }

    fn refresh_visible_page(&mut self, cx: &mut Context<Self>) {
        self.refresh_config_inputs(cx);
        if self.page == Page::Profiles {
            self.reload_profile_catalog(cx);
        }
        self.refresh(cx);
        if self.page == Page::Settings {
            self.refresh_core_management(cx);
            if !self.app_update.checked {
                self.refresh_app_update(cx);
            }
        }
        if self.page == Page::Traffic {
            self.refresh_traffic_history(cx);
        }
    }

    fn start_live_updates(
        mut activity: tokio::sync::watch::Receiver<bool>,
        log_revision: u64,
        traffic_revision: u64,
        cx: &mut Context<Self>,
    ) {
        let mut schedule = LiveUpdateSchedule::new(log_revision, traffic_revision);
        cx.spawn(async move |this, cx| {
            loop {
                let active = *activity.borrow_and_update();
                if !active {
                    if activity.changed().await.is_err() {
                        break;
                    }
                    if *activity.borrow_and_update()
                        && this
                            .update(cx, |this, _| {
                                schedule.synchronize(
                                    this.log_monitor.revision(),
                                    this.traffic_monitor.revision(),
                                );
                            })
                            .is_err()
                    {
                        break;
                    }
                    continue;
                }

                tokio::select! {
                    changed = activity.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        if *activity.borrow_and_update()
                            && this.update(cx, |this, _| {
                                schedule.synchronize(
                                    this.log_monitor.revision(),
                                    this.traffic_monitor.revision(),
                                );
                            }).is_err()
                        {
                            break;
                        }
                    }
                    () = tokio::time::sleep(LIVE_UPDATE_INTERVAL) => {
                        if this.update(cx, |this, cx| {
                            let actions = schedule.tick(
                                this.page,
                                this.log_monitor.revision(),
                                this.traffic_monitor.revision(),
                                !this.loading && !this.mutating,
                                this.connections.closing.is_empty(),
                            );
                            if actions.refresh_page {
                                this.refresh(cx);
                            }
                            if actions.refresh_history {
                                this.refresh_traffic_history(cx);
                            }
                            if actions.notify && !actions.refresh_page && !actions.refresh_history {
                                cx.notify();
                            }
                        }).is_err() {
                            break;
                        }
                    }
                }
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
                Err(error) => Err(zenclash_i18n::text_with(
                    "runtime.lifecycle.page_task",
                    &[("error", error.to_string())],
                )),
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
                        let token = this.page_task_token_for(page);
                        if this.replace_page_data(token, data) && page == Page::Network {
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
        success: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let success = success.into();
        let requested_log_level = patch
            .get("log-level")
            .and_then(Value::as_str)
            .and_then(MihomoLogLevel::from_api);
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some(zenclash_i18n::text("runtime.lifecycle.profile_missing"));
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
        let core_session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            let outcome = core_session
                .apply(
                    &controlled,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch,
                        overrides,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let controlled_config = controlled.load_json().map_err(|error| error.to_string())?;
            let data = load_page(client, page).await?;
            Ok::<_, String>((data, controlled_config, outcome.kind))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "runtime.lifecycle.config_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((data, controlled_config, apply_kind)) => {
                        if let Some(level) = requested_log_level {
                            this.log_monitor.set_level(level);
                        }
                        this.controlled_config_generation =
                            this.controlled_config_generation.wrapping_add(1);
                        this.controlled_config = controlled_config;
                        this.invalidate_config_inputs(cx);
                        this.config_preview = None;
                        cx.emit(RuntimeConfigApplied);
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if apply_kind == CoreApplyKind::Restarted {
                                let saved = success.replace(
                                    &zenclash_i18n::text("runtime.lifecycle.hot_reload_term"),
                                    &zenclash_i18n::text("runtime.lifecycle.save_term"),
                                );
                                zenclash_i18n::text_with(
                                    "runtime.lifecycle.restarted_after_save",
                                    &[
                                        ("message", saved),
                                        ("core", this.core_kind.display_name().to_owned()),
                                    ],
                                )
                            } else {
                                success
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

    pub(super) fn refresh_config_inputs(&mut self, cx: &mut Context<Self>) {
        if self.config_inputs_profile == self.profile_path || self.config_inputs_loading {
            return;
        }
        let Some(profile) = self.profile_path.clone() else {
            self.config_inputs_profile = None;
            return;
        };
        self.config_inputs_generation = self.config_inputs_generation.wrapping_add(1);
        let token = ConfigInputsTaskToken {
            profile: profile.clone(),
            generation: self.config_inputs_generation,
        };
        self.config_inputs_loading = true;
        let controlled = self.controlled_config_store.clone();
        let task = self.runtime.spawn_blocking(move || {
            controlled
                .effective_json(profile)
                .map(config_input_snapshot)
                .map_err(|error| error.to_string())
        });
        let window_handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if !token
                        .is_current(this.profile_path.as_deref(), this.config_inputs_generation)
                    {
                        return;
                    }
                    this.config_inputs_loading = false;
                    match result {
                        Ok(config) => {
                            this.config_inputs = ConfigInputs::new(&config, window, cx);
                            this.config_inputs_profile = Some(token.profile);
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    pub(super) fn invalidate_config_inputs(&mut self, cx: &mut Context<Self>) {
        self.config_inputs_generation = self.config_inputs_generation.wrapping_add(1);
        self.config_inputs_loading = false;
        self.config_inputs_profile = None;
        self.refresh_config_inputs(cx);
    }

    pub(crate) fn reload_controlled_config(&mut self, cx: &mut Context<Self>) {
        self.controlled_config_generation = self.controlled_config_generation.wrapping_add(1);
        let generation = self.controlled_config_generation;
        let controlled = self.controlled_config_store.clone();
        let task = self
            .runtime
            .spawn_blocking(move || controlled.load_json().map_err(|error| error.to_string()));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.controlled_config_generation != generation {
                    return;
                }
                match result {
                    Ok(controlled) => {
                        let changed = this.controlled_config != controlled;
                        this.controlled_config = controlled;
                        if changed {
                            this.invalidate_config_inputs(cx);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn profile_activated_from_tray(
        &mut self,
        path: std::path::PathBuf,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.profile_path = Some(path.clone());
        self.invalidate_config_inputs(cx);
        self.config_preview = None;
        self.reload_profile_catalog(cx);
        self.notice = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.tray_profile_selected",
            &[("name", name.to_owned())],
        ));
        cx.emit(super::ProfileActivated { path });
        self.refresh(cx);
    }

    pub(crate) fn report_tray_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.error = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.tray_profile_failed",
            &[("error", error.to_owned())],
        ));
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
        if let RuntimeData::Resources { proxy, rules, .. } = &data {
            self.provider_operations
                .observe_catalog(super::ProviderKind::Proxy, proxy);
            self.provider_operations
                .observe_catalog(super::ProviderKind::Rule, rules);
        }
        self.data = data.retain_dashboard_successes(&self.data);
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
            RuntimeData::Dashboard { config, .. } => config.value(),
            RuntimeData::Config(config)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_updates_require_both_an_active_window_and_presented_page() {
        let mut visibility = UiVisibility::new(true);
        visibility.page_presented = false;

        assert!(!visibility.updates_enabled());
    }

    #[test]
    fn hidden_window_disables_ui_updates_even_if_it_was_active() {
        let mut visibility = UiVisibility::new(true);
        visibility.window_visible = false;

        assert!(!visibility.updates_enabled());
    }

    #[test]
    fn idle_home_tick_does_not_request_a_repaint() {
        let mut schedule = LiveUpdateSchedule::new(2, 3);

        let actions = schedule.tick(Page::Home, 2, 3, true, true);

        assert_eq!(actions, LiveUpdateActions::default());
    }

    #[test]
    fn new_traffic_requests_a_home_repaint() {
        let mut schedule = LiveUpdateSchedule::new(2, 3);

        let actions = schedule.tick(Page::Home, 2, 4, true, true);

        assert!(actions.notify);
    }

    #[test]
    fn home_dashboard_refresh_keeps_its_five_second_cadence() {
        let mut schedule = LiveUpdateSchedule::new(0, 0);
        for _ in 0..9 {
            let _ = schedule.tick(Page::Home, 0, 0, true, true);
        }

        let actions = schedule.tick(Page::Home, 0, 0, true, true);

        assert!(actions.refresh_page);
    }

    #[test]
    fn synchronization_discards_revisions_accumulated_while_paused() {
        let mut schedule = LiveUpdateSchedule::new(1, 1);
        schedule.synchronize(5, 8);

        let actions = schedule.tick(Page::Logs, 5, 8, true, true);

        assert_eq!(actions, LiveUpdateActions::default());
    }
}
