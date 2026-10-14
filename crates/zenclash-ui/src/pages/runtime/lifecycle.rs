use super::{
    AppContext, ConfigInputs, Context, ControlledConfigStore, Duration, MihomoLogLevel, Page,
    PageSnapshotData, PageTaskToken, ProfileActivated, ProfileCatalog, ProfileStore, RuntimeConfig,
    RuntimeConfigApplied, RuntimePage, RuntimePageServices, Value, Window, YamlOverrideCatalog,
    YamlOverrideStore, config_input_snapshot,
};
use gpui_kit::component::WindowExt;
use zenclash_core::{CoreApplyKind, EffectiveConfigIntent};

const LIVE_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UiVisibility {
    window_active: bool,
    window_visible: bool,
    page_presented: bool,
}

impl UiVisibility {
    pub(super) const fn new(window_active: bool) -> Self {
        Self {
            window_active,
            window_visible: true,
            page_presented: true,
        }
    }

    pub(super) const fn page_visible(self) -> bool {
        self.window_visible && self.page_presented
    }

    const fn updates_enabled(self) -> bool {
        self.window_active && self.window_visible && self.page_presented
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LiveUpdateActions {
    notify: bool,
    notify_logs: bool,
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
            actions.notify_logs = true;
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
    override_store: Option<YamlOverrideStore>,
    override_catalog: YamlOverrideCatalog,
    error: Option<String>,
}

fn load_initial_persistent_state(
    controlled_store: &ControlledConfigStore,
    profile_store: Option<ProfileStore>,
    override_store: Option<YamlOverrideStore>,
) -> InitialPersistentState {
    let (profile_store, profile_catalog, store_error) = match profile_store {
        Some(store) => match store.load() {
            Ok(catalog) => (Some(store), catalog, None),
            Err(error) => (
                Some(store),
                ProfileCatalog::default(),
                Some(error.to_string()),
            ),
        },
        None => (
            None,
            ProfileCatalog::default(),
            Some(zenclash_i18n::text("profiles.errors.store_unavailable")),
        ),
    };
    let (controlled_config, controlled_error) = controlled_store.load_json().map_or_else(
        |error| (empty_json_object(), Some(error.to_string())),
        |value| (value, None),
    );
    let (override_store, override_catalog, override_error) = match override_store {
        Some(store) => match store.load() {
            Ok(catalog) => (Some(store), catalog, None),
            Err(error) => (
                Some(store),
                YamlOverrideCatalog::default(),
                Some(error.to_string()),
            ),
        },
        None => (
            None,
            YamlOverrideCatalog::default(),
            Some(zenclash_i18n::text("overrides.errors.store_unavailable")),
        ),
    };
    InitialPersistentState {
        profile_store,
        profile_catalog,
        controlled_config,
        override_store,
        override_catalog,
        error: store_error.or(controlled_error).or(override_error),
    }
}

fn empty_json_object() -> Value {
    Value::Object(serde_json::Map::default())
}

impl RuntimePage {
    pub(crate) fn report_automatic_runtime(
        &mut self,
        notice: Option<&str>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(error) = error {
            self.error = Some(zenclash_i18n::text_with(
                "automatic.failed",
                &[("error", error)],
            ));
        }
        if let Some(notice) = notice {
            self.notice = Some(zenclash_i18n::text(notice));
            self.invalidate_config_inputs(cx);
            self.refresh(cx);
        }
        cx.notify();
    }

    pub(crate) fn preferences_restored_from_app(
        &mut self,
        preferences: super::AppPreferences,
        previews: crate::preference_previews::PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        self.preferences = preferences;
        self.preference_previews = previews;
        self.preference_previews.apply(&mut self.preferences);
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
        self.connections.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        self.logs.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        self.rules.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        let localized_inputs = [
            (
                self.network_diagnostics.latency_name.clone(),
                "runtime.placeholders.network_target",
            ),
            (
                self.network_diagnostics.dns_name.clone(),
                "runtime.placeholders.dns_name",
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
                self.config_inputs.tun.device.clone(),
                "config_inputs.placeholders.tun_device",
            ),
            (
                self.config_inputs.tun.mtu.clone(),
                "config_inputs.placeholders.default_mtu",
            ),
        ];
        for (input, key) in localized_inputs {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
        let localized_textareas = [
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
                self.config_inputs.tun.route_include_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
            (
                self.config_inputs.tun.route_exclude_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
        ];
        for (input, key) in localized_textareas {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
        self.profiles
            .forms
            .refresh_localized_placeholders(window, cx);
        self.profiles
            .forms
            .yaml_editor
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
        outcome: crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) {
        let name = outcome.name().to_owned();
        self.synchronize_profile_receipt(&outcome, cx);
        self.notice = Some(outcome.warning().unwrap_or_else(|| {
            zenclash_i18n::text_with("runtime.lifecycle.profile_updated", &[("name", name)])
        }));
        cx.notify();
    }

    pub(super) fn synchronize_profile_receipt(
        &mut self,
        receipt: &crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) -> bool {
        self.reload_profile_catalog(cx);
        self.synchronize_profile_recovery();
        let Some(runtime_version) = receipt.runtime_version() else {
            return false;
        };
        let accepted = self.application_commands.is_current(runtime_version);
        // A later mode/upgrade transition may invalidate the receipt without
        // changing its profile. Publish current core truth instead of losing
        // the only path notification from that completed profile transaction.
        self.synchronize_committed_profile(cx);
        accepted
    }

    pub(super) fn synchronize_committed_profile(&mut self, cx: &mut Context<Self>) {
        self.synchronize_profile_recovery();
        let snapshot = self.application_commands.committed_profile();
        let Some(path) = snapshot.profile_path else {
            return;
        };
        self.profile_path = Some(path.clone());
        self.invalidate_config_inputs(cx);
        self.overrides.invalidate_preview();
        cx.emit(ProfileActivated {
            path,
            runtime_version: snapshot.generation,
        });
    }

    pub(super) fn synchronize_profile_recovery(&mut self) {
        self.profiles
            .synchronize_recovery(&self.application_commands);
    }

    pub(crate) fn report_background_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.synchronize_profile_recovery();
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
            profile_store,
            override_store,
            core_kind,
            core_session,
            profile_service: application_commands,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            operational_status,
            traffic_capture,
            profile_path,
            controlled_config_store,
            preferences_store,
            preferences,
            system_proxy_session,
            traffic_history_store,
            startup_notice,
            startup_error,
        } = services;
        let initial_runtime_version = core_session.generation();
        let persistent_task = {
            let store = controlled_config_store.clone();
            let profiles = profile_store.clone();
            let overrides = override_store.clone();
            runtime
                .spawn_blocking(move || load_initial_persistent_state(&store, profiles, overrides))
        };
        let profile_catalog = ProfileCatalog::default();
        let controlled_config = empty_json_object();
        let effective_config = empty_json_object();
        let override_catalog = YamlOverrideCatalog::default();
        let error = None;
        let effective_config = config_input_snapshot(effective_config);
        let mut config_inputs = ConfigInputs::new(&effective_config, None, window, cx);
        config_inputs.begin_bootstrap_read();
        let profile_forms = super::profiles::ProfileFormState::new(window, cx);
        let provider_operations = super::ProviderOperations::new(client.clone());
        let network_diagnostics = super::network::NetworkDiagnosticsUiState::new(window, cx);
        let connections =
            cx.new(|cx| super::connections::ConnectionsView::new(runtime.clone(), window, cx));
        let connection_events = cx.subscribe(
            &connections,
            |this, _, event: &super::connections::ConnectionEvent, cx| {
                this.handle_connection_event(event.clone(), cx)
            },
        );
        let connection_presentation = cx.observe(&connections, |_, _, cx| cx.notify());
        let connection_availability = cx.observe_self(|this, cx| {
            let allowed = !this.core_busy();
            this.connections
                .update(cx, |view, cx| view.set_close_allowed(allowed, cx));
        });
        let logs = cx.new(|cx| {
            super::logs::LogsView::new(runtime.clone(), log_monitor.clone(), core_kind, window, cx)
        });
        let log_events = cx.subscribe(&logs, |this, _, event: &super::logs::LogEvent, cx| {
            this.handle_log_event(event.clone(), cx);
        });
        let log_presentation = cx.observe(&logs, |_, _, cx| cx.notify());
        let log_context = cx.observe_self(|this, cx| this.synchronize_log_context(cx));
        let rules = cx.new(|cx| super::rules::RulesView::new(runtime.clone(), window, cx));
        let rule_events = cx.subscribe(&rules, |this, _, event: &super::rules::RuleEvent, cx| {
            this.handle_rule_event(event.clone(), cx)
        });
        let rule_presentation = cx.observe(&rules, |_, _, cx| cx.notify());
        let rule_context = cx.observe_self(|this, cx| this.synchronize_rule_context(cx));
        let resources = super::resources::ResourcesState::new(window, cx);
        let ui_visibility = UiVisibility::new(window.is_window_active());
        let (live_updates_enabled, live_update_activity) =
            tokio::sync::watch::channel(ui_visibility.updates_enabled());
        let traffic_history =
            super::traffic::TrafficHistory::new(runtime.clone(), traffic_history_store);
        let mut this = Self {
            feedback_notifications: Default::default(),
            page,
            core_kind,
            core_session,
            application_commands,
            client,
            runtime,
            traffic_monitor,
            log_monitor,
            operational_status,
            traffic_capture,
            profile_path,
            controlled_config_store,
            controlled_config,
            config_inputs,
            persistent_loading: true,
            preferences_store,
            preferences,
            preference_previews: Default::default(),
            core_management: super::settings::CoreManagementUiState::default(),
            body_scroll: gpui_kit::ScrollHandle::default(),
            resources,
            system_proxy_session,
            profiles: super::profiles::ProfileLibrary::new(
                profile_store,
                profile_catalog,
                profile_forms,
            ),
            overrides: super::overrides::OverridesState::new(override_store, override_catalog),
            connections,
            logs,
            rules,
            system_proxy_editor: None,
            core_releases: super::CoreReleaseState::default(),
            page_observation: super::page_observation::PageObservationOwner::new(
                initial_runtime_version,
            ),
            home: super::home::HomeUiState::default(),
            traffic_history,
            network_diagnostics,
            provider_operations,
            navigation_generation: 0,
            controlled_config_generation: 0,
            loading: false,
            mutations: super::busy::MutationState::default(),
            error,
            startup_error,
            notice: startup_notice,
            focus_handle: cx.focus_handle(),
            window_handle: window.window_handle(),
            ui_visibility,
            live_updates_enabled,
            _subscriptions: vec![
                connection_events,
                connection_presentation,
                connection_availability,
                log_events,
                log_presentation,
                log_context,
                rule_events,
                rule_presentation,
                rule_context,
            ],
        };
        let window_activation_subscription =
            cx.observe_window_activation(window, |this, window, cx| {
                this.set_window_active(window.is_window_active(), cx);
            });
        this._subscriptions.push(window_activation_subscription);
        let feedback_subscription = cx.observe_in(&cx.entity(), window, |this, _, window, cx| {
            this.publish_feedback(window, cx);
        });
        this._subscriptions.push(feedback_subscription);
        cx.defer_in(window, |this, window, cx| this.publish_feedback(window, cx));
        this.synchronize_log_context(cx);
        this.synchronize_rule_context(cx);
        this.finish_initial_persistent_state(persistent_task, initial_runtime_version, cx);
        this.refresh(cx);
        if let Some(mut updates) = this.application_commands.service_updates() {
            cx.spawn(async move |this, cx| {
                while updates.changed().await.is_ok() {
                    if this
                        .update(cx, |this, cx| {
                            if matches!(this.page, Page::Tun | Page::Home) {
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

    fn finish_initial_persistent_state(
        &self,
        task: tokio::task::JoinHandle<InitialPersistentState>,
        runtime_version: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.persistent_loading = false;
                this.config_inputs.finish_bootstrap_read();
                match result {
                    Ok(state) => {
                        this.profiles
                            .adopt_bootstrap(state.profile_store, state.profile_catalog);
                        this.overrides
                            .adopt_bootstrap(state.override_store, state.override_catalog);
                        if this.application_commands.is_current(runtime_version) {
                            this.controlled_config = state.controlled_config;
                            this.error = state.error;
                        } else {
                            this.reload_controlled_config(cx);
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                this.refresh_config_inputs(cx);
                cx.notify();
            });
        })
        .detach();
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
                let capture = updates.borrow().capture.clone();
                if this
                    .update(cx, |this, cx| {
                        this.reconcile_home_generation();
                        this.reconcile_home_capture_transition(&capture);
                        if this.live_updates_enabled()
                            && matches!(this.page, Page::Home | Page::Tun)
                        {
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

    pub(super) fn restore_page_focus(&self, page: Page, cx: &mut Context<Self>) {
        let generation = self.navigation_generation;
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                if this.page != page
                    || this.navigation_generation != generation
                    || !this.ui_visibility.page_presented
                {
                    return;
                }
                let focus = this.focus_handle.clone();
                let _ = cx.update_window(this.window_handle, |_, window, cx| {
                    focus.focus(window, cx);
                });
            });
        });
    }

    /// Switches the active tab and invalidates results from older page tasks.
    pub fn switch_to(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        let previous_page = self.page;
        let close_subscription =
            previous_page == Page::Profiles && self.profiles.forms.close_subscription();

        let config = matches!(page, Page::Dns | Page::Sniffer)
            .then(|| self.config().cloned())
            .flatten();
        let config_observation = self.page_observation.source().clone();
        self.retain_page_snapshot(cx);
        if self.page == Page::Network {
            self.cancel_network_diagnostics();
        }
        if previous_page == Page::Logs {
            self.logs.update(cx, |view, cx| view.release_results(cx));
        }
        if previous_page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if previous_page == Page::Override {
            self.overrides.invalidate_preview();
        }
        self.page = page;
        if previous_page == Page::Home {
            self.release_home_presentation();
        }
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        let generation = self.navigation_generation;
        let page = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = page.update(cx, |page, cx| {
                if page.navigation_generation != generation || !page.ui_visibility.page_presented {
                    return;
                }
                let focus = page.focus_handle.clone();
                let _ = cx.update_window(page.window_handle, |_, window, cx| {
                    if close_subscription {
                        window.close_dialog(cx);
                    }
                    focus.focus(window, cx);
                });
            });
        });
        self.connections
            .update(cx, |view, _| view.release_presentation());
        self.rules
            .update(cx, |view, cx| view.release_presentation(cx));
        self.invalidate_page_load();
        let runtime_version = self.core_session.generation();
        self.restore_page_observation();
        self.page_observation
            .reuse_config(config, config_observation, runtime_version);
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.update_rule_presentation(cx);
        self.error = None;
        self.notice = None;
        if self.live_updates_enabled() {
            self.refresh_visible_page(cx);
        }
        cx.notify();
    }

    pub(crate) fn set_presented(&mut self, presented: bool, cx: &mut Context<Self>) {
        if self.ui_visibility.page_presented == presented {
            return;
        }
        if !presented && !self.core_busy() {
            self.release_inactive_page_data(cx);
        }
        let was_enabled = self.ui_visibility.updates_enabled();
        self.ui_visibility.page_presented = presented;
        self.update_live_update_activity(was_enabled, cx);
    }

    pub(crate) fn set_window_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.ui_visibility.window_visible == visible {
            return;
        }
        if !visible && !self.core_busy() {
            self.release_inactive_page_data(cx);
            self.page_observation.clear_cache();
        }
        let was_enabled = self.ui_visibility.updates_enabled();
        self.ui_visibility.window_visible = visible;
        self.update_live_update_activity(was_enabled, cx);
    }

    fn retain_page_snapshot(&mut self, cx: &mut Context<Self>) {
        if !self.core_busy() && self.rules.read(cx).is_snapshot_retainable() {
            self.page_observation.retain_current(self.page);
        } else {
            self.page_observation.release_data();
        }
    }

    fn release_inactive_page_data(&mut self, cx: &mut Context<Self>) {
        self.retain_page_snapshot(cx);
        self.release_home_presentation();
        self.logs.update(cx, |view, cx| view.release_results(cx));
        if self.page == Page::Network {
            self.cancel_network_diagnostics();
        }
        if self.page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if self.page == Page::Override {
            self.overrides.invalidate_preview();
        }
        self.invalidate_page_load();
        self.page_observation.release_data();
        self.connections
            .update(cx, |view, _| view.release_presentation());
        self.rules
            .update(cx, |view, cx| view.release_presentation(cx));
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
        if !enabled {
            if !self.ui_visibility.window_visible || !self.ui_visibility.page_presented {
                self.release_home_presentation();
                self.connections
                    .update(cx, |view, _| view.release_presentation());
            }
            if self.network_diagnostics.query.is_loading() {
                self.cancel_network_diagnostics();
            }
            self.logs
                .update(cx, |view, cx| view.suspend_presentation(cx));
            self.traffic_history.cancel_query();
            if self.ui_visibility.window_visible && self.ui_visibility.page_presented {
                self.overrides.cancel_preview();
                self.rules
                    .update(cx, |view, cx| view.suspend_presentation(cx));
            } else {
                self.overrides.invalidate_preview();
                self.rules
                    .update(cx, |view, cx| view.release_presentation(cx));
            }
        }
        if was_enabled == enabled {
            if self.ui_visibility.window_visible && self.ui_visibility.page_presented {
                self.update_connection_presentation(cx);
            }
            return;
        }
        self.live_updates_enabled.send_replace(enabled);
        if enabled {
            if matches!(self.page_observation.data(), PageSnapshotData::Empty) {
                self.page_observation
                    .restore_for_resume(self.page, &self.core_session);
                self.prepare_home_projection();
            }
            self.update_connection_presentation(cx);
            self.refresh_visible_page(cx);
            self.update_rule_presentation(cx);
            cx.notify();
        }
    }

    pub(crate) const fn live_updates_enabled(&self) -> bool {
        self.ui_visibility.updates_enabled()
    }

    fn refresh_visible_page(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Home {
            // Restore the real stream backlog before waiting for controller refreshes.
            self.update_home_traffic_presentation(cx);
        }
        self.refresh_config_inputs(cx);
        if self.page == Page::Profiles {
            self.reload_profile_catalog(cx);
        }
        self.refresh(cx);
        if self.page == Page::Mihomo {
            self.refresh_core_management(cx);
        }
        if self.page == Page::Traffic {
            self.refresh_traffic_history(cx);
        }
        if self.page == Page::Logs {
            self.update_log_presentation(cx);
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
                    () = cx.background_executor().timer(LIVE_UPDATE_INTERVAL) => {
                        if this.update(cx, |this, cx| {
                            let actions = schedule.tick(
                                this.page,
                                this.log_monitor.revision(),
                                this.traffic_monitor.revision(),
                                !this.loading && !this.core_busy(),
                                this.connections.read(cx).is_idle(),
                            );
                            if this.page == Page::Home {
                                this.update_home_traffic_presentation(cx);
                            }
                            if actions.refresh_page {
                                this.refresh(cx);
                            }
                            if actions.refresh_history {
                                this.refresh_traffic_history(cx);
                            }
                            if this.page == Page::Logs {
                                this.update_log_presentation(cx);
                            }
                            if actions.notify && !actions.notify_logs && !actions.refresh_page && !actions.refresh_history {
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
        self.refresh_config_inputs(cx);
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let page = self.page;
        let request = self
            .page_observation
            .begin(page, self.navigation_generation);
        let session = self.core_session.clone();
        let read_source =
            std::sync::Arc::new(crate::runtime_read_source::RuntimeReadSource::default());
        let submitted_source = read_source.clone();
        let task = self.runtime.spawn(async move {
            super::page_observation::ObservedPage::load_with_source(
                session,
                page,
                &submitted_source,
            )
            .await
        });
        self.page_observation.track(request, &task);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this
                    .page_observation
                    .finish(request, this.page, this.navigation_generation)
                {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(observed) => {
                        if this.accept_page_observation(observed, cx) && page == Page::Network {
                            this.refresh_network_diagnostics(cx);
                        }
                    }
                    Err(error) => {
                        if this.startup_error.is_none() {
                            this.publish_page_refresh_error(&read_source, error);
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

    pub(super) fn publish_page_refresh_error(
        &mut self,
        source: &crate::runtime_read_source::RuntimeReadSource,
        error: String,
    ) -> bool {
        match source.identity() {
            Some(identity) => {
                identity.belongs_to(&self.core_session)
                    && identity
                        .publish_if_current(|| self.error = Some(error))
                        .is_some()
            }
            // The caller checks the original query; no controller I/O began before capture.
            None => {
                self.error = Some(error);
                true
            }
        }
    }

    fn restore_page_observation(&mut self) {
        self.page_observation
            .restore_current(self.page, &self.core_session);
    }

    pub(super) fn accept_page_observation(
        &mut self,
        observed: super::page_observation::ObservedPage,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut data_accepted = false;
        let accepted = observed.publish(|result, runtime| {
            let data = match result {
                Ok(data) => data,
                Err(error) => {
                    if self.startup_error.is_none() {
                        self.error = Some(error);
                    }
                    return;
                }
            };
            self.page_observation.adopt(data, runtime);
            self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
            data_accepted = true;
        });
        if accepted {
            if !data_accepted {
                return false;
            }
            self.update_observed_page_presentation(cx);
        } else {
            self.refresh(cx);
        }
        accepted
    }

    pub(super) fn update_observed_page_presentation(&mut self, cx: &mut Context<Self>) {
        if let PageSnapshotData::Resources { proxy, rules, .. } = self.page_observation.data()
            && let Some(identity) = self.page_observation.source().as_ref()
        {
            self.provider_operations.observe_catalog_for_runtime(
                super::ProviderKind::Proxy,
                proxy,
                identity,
            );
            self.provider_operations.observe_catalog_for_runtime(
                super::ProviderKind::Rule,
                rules,
                identity,
            );
        }
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.update_rule_presentation(cx);
    }

    pub(super) fn apply_controlled_config(
        &mut self,
        patch: Value,
        success: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if !self
            .config_inputs
            .is_for_profile(self.profile_path.as_deref())
        {
            self.error = Some(zenclash_i18n::text("runtime.empty.loading"));
            cx.notify();
            return;
        }
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
        let submitted_inputs = self.config_inputs.submitted(&patch, cx);
        let controlled = self.controlled_config_store.clone();
        let overrides = self.enabled_override_paths();
        let commands = self.application_commands.clone();
        let task = self.runtime.spawn(async move {
            let outcome = commands
                .apply_config(
                    &controlled,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch,
                        overrides,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(outcome)
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
                this.finish_mutation(token);
                match result {
                    Ok(outcome) => {
                        if !this.application_commands.is_current(outcome.generation) {
                            this.reload_controlled_config(cx);
                            this.invalidate_config_inputs(cx);
                            this.invalidate_page_load();
                            this.refresh(cx);
                            return;
                        }
                        this.config_inputs.accept_submitted(submitted_inputs, cx);
                        if let Some(level) = requested_log_level {
                            this.log_monitor.set_level(level);
                        }
                        this.reload_controlled_config(cx);
                        this.overrides.invalidate_preview();
                        cx.emit(RuntimeConfigApplied);
                        this.invalidate_page_load();
                        this.refresh(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if outcome.kind == CoreApplyKind::Restarted {
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
        self.config_inputs
            .effective()
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
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
                        this.controlled_config = controlled;
                        this.invalidate_config_inputs(cx);
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
        receipt: crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) {
        let name = receipt.name().to_owned();
        if self.synchronize_profile_receipt(&receipt, cx) {
            self.notice = Some(receipt.warning().unwrap_or_else(|| {
                zenclash_i18n::text_with(
                    "runtime.lifecycle.tray_profile_selected",
                    &[("name", name)],
                )
            }));
            self.refresh(cx);
        }
        cx.notify();
    }

    pub(crate) fn report_tray_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.synchronize_profile_recovery();
        self.error = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.tray_profile_failed",
            &[("error", error.to_owned())],
        ));
        cx.notify();
    }

    pub(super) fn core_busy(&self) -> bool {
        self.mutation_busy(super::busy::MutationDomain::Core)
    }

    pub(super) fn mutation_busy(&self, domain: super::busy::MutationDomain) -> bool {
        self.persistent_loading || self.mutations.busy(domain)
    }

    pub(super) fn begin_mutation(&mut self, page: Page) -> Option<PageTaskToken> {
        self.begin_scoped_mutation(page, super::busy::MutationDomain::Core)
    }

    pub(super) fn begin_scoped_mutation(
        &mut self,
        page: Page,
        domain: super::busy::MutationDomain,
    ) -> Option<PageTaskToken> {
        if self.persistent_loading {
            return None;
        }
        let mutation = self.mutations.begin(domain)?;
        self.page_observation.clear_cache();
        if matches!(
            domain,
            super::busy::MutationDomain::Core
                | super::busy::MutationDomain::Backup
                | super::busy::MutationDomain::BackupRecovery
        ) {
            self.invalidate_page_load();
        }
        self.error = None;
        self.notice = None;
        Some(PageTaskToken {
            mutation: Some(mutation),
            ..self.page_task_token_for(page)
        })
    }

    pub(super) fn finish_mutation(&mut self, token: PageTaskToken) {
        self.synchronize_profile_recovery();
        if let Some(mutation) = token.mutation {
            self.mutations.finish(mutation);
        }
    }

    pub(super) fn invalidate_page_load(&mut self) {
        self.page_observation.cancel();
        self.loading = false;
    }

    pub(super) const fn page_task_token_for(&self, page: Page) -> PageTaskToken {
        PageTaskToken {
            page,
            navigation_generation: self.navigation_generation,
            mutation: None,
        }
    }

    pub(super) fn is_page_task_current(&self, token: PageTaskToken) -> bool {
        token.is_current(self.page, self.navigation_generation)
    }

    pub(super) fn replace_page_data(
        &mut self,
        token: PageTaskToken,
        data: PageSnapshotData,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_page_task_current(token) {
            return false;
        }
        if let PageSnapshotData::Resources { proxy, rules, .. } = &data {
            self.provider_operations
                .observe_catalog(super::ProviderKind::Proxy, proxy);
            self.provider_operations
                .observe_catalog(super::ProviderKind::Rule, rules);
        }
        self.page_observation
            .replace_unobserved(data, self.core_session.generation());
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
        self.update_rule_presentation(cx);
        true
    }

    pub(super) fn set_page_error(&mut self, token: PageTaskToken, error: String) {
        if self.is_page_task_current(token) {
            self.error = Some(error);
        } else {
            tracing::warn!(page = ?token.page, %error, "discarded error from an inactive page task");
        }
    }

    pub(super) fn config(&self) -> Option<&RuntimeConfig> {
        self.page_observation.config(self.core_session.generation())
    }

    pub(super) fn mihomo_binary(&self) -> Option<std::path::PathBuf> {
        self.core_session
            .runtime_descriptor()
            .binary()
            .map(std::path::Path::to_path_buf)
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

#[cfg(test)]
mod ui_tests;
