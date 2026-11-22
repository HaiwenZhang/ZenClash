use crate::features::host::{
    LibraryPagesState, ProfileActivated, ProfileCleared, RuntimeConfigApplied, RuntimePage,
    RuntimePageServices, SettingsWorkspaceState, ShellPresentationState,
};
use crate::foundation::actions::Page;
use crate::foundation::primitives::{
    preference_previews::PreferencePreviews, runtime_read_source::RuntimeReadSource,
};
use crate::runtime_host::{
    busy::{MutationDomain, MutationState},
    config_inputs::{ConfigInputs, config_input_snapshot},
    lifecycle_schedule::{LIVE_UPDATE_INTERVAL, LiveUpdateSchedule, UiVisibility},
    observation::{ObservedPage, PageObservationOwner, PageSnapshotData},
    services::RuntimeMonitors,
    task_token::PageTaskToken,
};
use gpui_kit::component::WindowExt;
use gpui_kit::{AppContext, Context, Window};
use serde_json::Value;
use zenclash_core::{
    AppPreferences, ControlledConfigStore, CoreApplyKind, MihomoLogLevel, ProfileCatalog,
    ProfileStore, ProviderKind, ProviderOperations, RuntimeConfig, YamlOverrideCatalog,
    YamlOverrideStore,
};

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
        if let Some(notice) = notice {
            self.notice = Some(zenclash_i18n::text(notice));
            self.invalidate_config_inputs(cx);
            self.refresh(cx);
        }
        if let Some(error) = error {
            self.error = Some(zenclash_i18n::text_with(
                "automatic.failed",
                &[("error", error)],
            ));
        }
        cx.notify();
    }

    pub(crate) fn preferences_restored_from_app(
        &mut self,
        preferences: AppPreferences,
        previews: PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        self.preferences = preferences;
        self.settings.preference_previews = previews;
        self.settings
            .preference_previews
            .apply(&mut self.preferences);
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

    pub(in crate::features) fn synchronize_profile_receipt(
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

    pub(in crate::features) fn synchronize_committed_profile(&mut self, cx: &mut Context<Self>) {
        self.synchronize_committed_profile_state(false, cx);
    }

    pub(in crate::features) fn synchronize_cleared_or_fallback_profile(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.synchronize_committed_profile_state(true, cx);
    }

    fn synchronize_committed_profile_state(
        &mut self,
        emit_when_cleared: bool,
        cx: &mut Context<Self>,
    ) {
        self.synchronize_profile_recovery();
        let snapshot = self.application_commands.committed_profile();
        match snapshot.profile_path {
            Some(path) => {
                self.profile_path = Some(path.clone());
                self.invalidate_config_inputs(cx);
                self.library.overrides.invalidate_preview();
                cx.emit(ProfileActivated {
                    path,
                    runtime_version: snapshot.generation,
                });
            }
            None if emit_when_cleared => {
                self.profile_path = None;
                self.invalidate_config_inputs(cx);
                self.library.overrides.invalidate_preview();
                cx.emit(ProfileCleared {
                    runtime_version: snapshot.generation,
                });
            }
            None => {}
        }
    }

    pub(in crate::features) fn synchronize_profile_recovery(&mut self) {
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
        let profile_forms = crate::features::profiles::ProfileFormState::new(window, cx);
        let provider_operations = ProviderOperations::new(client.clone());
        let network_diagnostics =
            crate::features::network::NetworkDiagnosticsUiState::new(window, cx);
        let connections = cx.new(|cx| {
            crate::features::connections::ConnectionsView::new(runtime.clone(), window, cx)
        });
        let connection_events = cx.subscribe(
            &connections,
            |this, _, event: &crate::features::connections::ConnectionEvent, cx| {
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
            crate::features::logs::LogsView::new(
                runtime.clone(),
                log_monitor.clone(),
                core_kind,
                window,
                cx,
            )
        });
        let log_events = cx.subscribe(
            &logs,
            |this, _, event: &crate::features::logs::LogEvent, cx| {
                this.handle_log_event(event.clone(), cx);
            },
        );
        let log_presentation = cx.observe(&logs, |_, _, cx| cx.notify());
        let log_context = cx.observe_self(|this, cx| this.synchronize_log_context(cx));
        let rules =
            cx.new(|cx| crate::features::rules::RulesView::new(runtime.clone(), window, cx));
        let rule_events = cx.subscribe(
            &rules,
            |this, _, event: &crate::features::rules::RuleEvent, cx| {
                this.handle_rule_event(event.clone(), cx)
            },
        );
        let rule_presentation = cx.observe(&rules, |_, _, cx| cx.notify());
        let rule_context = cx.observe_self(|this, cx| this.synchronize_rule_context(cx));
        let resources = crate::features::resources::ResourcesState::new(window, cx);
        let ui_visibility = UiVisibility::new(window.is_visible());
        let (live_updates_enabled, live_update_activity) =
            tokio::sync::watch::channel(ui_visibility.updates_enabled());
        let traffic_history =
            crate::features::traffic::TrafficHistory::new(runtime.clone(), traffic_history_store);
        let mut this = Self {
            page,
            core_kind,
            core_session,
            application_commands,
            client,
            runtime,
            monitors: RuntimeMonitors {
                traffic_monitor,
                log_monitor,
                operational_status,
                traffic_capture,
                provider_operations,
            },
            settings: SettingsWorkspaceState {
                controlled_config,
                controlled_config_generation: 0,
                core_management: crate::features::settings::CoreManagementUiState::default(),
                core_releases: crate::features::kernel::mihomo::CoreReleaseState::default(),
                system_proxy_editor: None,
                system_proxy_session,
                preference_previews: Default::default(),
            },
            library: LibraryPagesState {
                resources,
                overrides: crate::features::overrides::OverridesState::new(
                    override_store,
                    override_catalog,
                ),
            },
            shell: ShellPresentationState {
                feedback_notifications: Default::default(),
                body_scroll: gpui_kit::ScrollHandle::default(),
                window_handle: window.window_handle(),
                ui_visibility,
                live_updates_enabled,
                startup_error,
                tasks: Vec::new(),
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
            },
            profile_path,
            controlled_config_store,
            config_inputs,
            persistent_loading: true,
            preferences_store,
            preferences,
            profiles: crate::features::profiles::ProfileLibrary::new(
                profile_store,
                profile_catalog,
                profile_forms,
            ),
            connections,
            logs,
            rules,
            page_observation: PageObservationOwner::new(initial_runtime_version),
            home: crate::features::home::HomeUiState::default(),
            traffic_history,
            network_diagnostics,
            navigation_generation: 0,
            loading: false,
            mutations: MutationState::default(),
            error,
            notice: startup_notice,
            focus_handle: cx.focus_handle(),
        };
        let window_visibility_subscription =
            cx.observe_window_visibility(window, |this, visibility, _, cx| {
                this.set_window_visible(visibility.is_visible(), cx);
            });
        this.shell
            ._subscriptions
            .push(window_visibility_subscription);
        let window_activation_subscription = cx.observe_window_activation(window, |this, _, cx| {
            if this.page == Page::Home && this.live_updates_enabled() {
                cx.notify();
            }
        });
        this.shell
            ._subscriptions
            .push(window_activation_subscription);
        let feedback_subscription = cx.observe_in(&cx.entity(), window, |this, _, window, cx| {
            this.publish_feedback(window, cx);
        });
        this.shell._subscriptions.push(feedback_subscription);
        cx.defer_in(window, |this, window, cx| this.publish_feedback(window, cx));
        this.synchronize_log_context(cx);
        this.synchronize_rule_context(cx);
        this.finish_initial_persistent_state(persistent_task, initial_runtime_version, cx);
        this.refresh(cx);
        if let Some(mut updates) = this.application_commands.service_updates() {
            let service_task = cx.spawn(async move |this, cx| {
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
            });
            this.shell.tasks.push(service_task);
        }
        let operational_task = Self::start_operational_updates(
            this.monitors.operational_status.subscribe(),
            this.shell.live_updates_enabled.subscribe(),
            cx,
        );
        this.shell.tasks.push(operational_task);
        let live_updates_task = Self::start_live_updates(
            live_update_activity,
            this.monitors.log_monitor.revision(),
            this.monitors.traffic_monitor.revision(),
            cx,
        );
        this.shell.tasks.push(live_updates_task);
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
                        this.library
                            .overrides
                            .adopt_bootstrap(state.override_store, state.override_catalog);
                        if this.application_commands.is_current(runtime_version) {
                            this.settings.controlled_config = state.controlled_config;
                            // A successful bootstrap must preserve feedback from a concurrent page read.
                            if let Some(error) = state.error {
                                this.error = Some(error);
                            }
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
    ) -> gpui_kit::Task<()> {
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
    }

    pub(in crate::features) fn restore_page_focus(&self, page: Page, cx: &mut Context<Self>) {
        let generation = self.navigation_generation;
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                if this.page != page
                    || this.navigation_generation != generation
                    || !this.shell.ui_visibility.page_presented
                {
                    return;
                }
                let focus = this.focus_handle.clone();
                let _ = cx.update_window(this.shell.window_handle, |_, window, cx| {
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
            self.library.overrides.invalidate_preview();
        }
        self.page = page;
        if !page.descriptor().owns_viewport_scroll() {
            self.shell
                .body_scroll
                .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        }
        if previous_page == Page::Home {
            self.release_home_presentation();
        }
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        let generation = self.navigation_generation;
        let page = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = page.update(cx, |page, cx| {
                if page.navigation_generation != generation
                    || !page.shell.ui_visibility.page_presented
                {
                    return;
                }
                let focus = page.focus_handle.clone();
                let _ = cx.update_window(page.shell.window_handle, |_, window, cx| {
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
        if self.shell.ui_visibility.page_presented == presented {
            return;
        }
        if !presented && !self.core_busy() {
            self.release_inactive_page_data(cx);
        }
        let was_enabled = self.shell.ui_visibility.updates_enabled();
        self.shell.ui_visibility.page_presented = presented;
        self.update_live_update_activity(was_enabled, cx);
    }

    pub(crate) fn set_window_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.shell.ui_visibility.window_visible == visible {
            return;
        }
        if !visible && !self.core_busy() {
            self.release_inactive_page_data(cx);
            self.page_observation.clear_cache();
        }
        let was_enabled = self.shell.ui_visibility.updates_enabled();
        self.shell.ui_visibility.window_visible = visible;
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
            self.library.overrides.invalidate_preview();
        }
        self.invalidate_page_load();
        self.page_observation.release_data();
        self.connections
            .update(cx, |view, _| view.release_presentation());
        self.rules
            .update(cx, |view, cx| view.release_presentation(cx));
    }

    fn update_live_update_activity(&mut self, was_enabled: bool, cx: &mut Context<Self>) {
        let enabled = self.shell.ui_visibility.updates_enabled();
        if !enabled {
            self.release_home_presentation();
            self.connections
                .update(cx, |view, _| view.release_presentation());
            if self.network_diagnostics.query.is_loading() {
                self.cancel_network_diagnostics();
            }
            self.logs
                .update(cx, |view, cx| view.suspend_presentation(cx));
            self.traffic_history.cancel_query();
            self.library.overrides.invalidate_preview();
            self.rules
                .update(cx, |view, cx| view.release_presentation(cx));
        }
        if was_enabled == enabled {
            if self.shell.ui_visibility.window_visible && self.shell.ui_visibility.page_presented {
                self.update_connection_presentation(cx);
            }
            return;
        }
        self.shell.live_updates_enabled.send_replace(enabled);
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
        self.shell.ui_visibility.updates_enabled()
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
    ) -> gpui_kit::Task<()> {
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
                                    this.monitors.log_monitor.revision(),
                                    this.monitors.traffic_monitor.revision(),
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
                                    this.monitors.log_monitor.revision(),
                                    this.monitors.traffic_monitor.revision(),
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
                                this.monitors.log_monitor.revision(),
                                this.monitors.traffic_monitor.revision(),
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
    }

    pub(in crate::features) fn refresh(&mut self, cx: &mut Context<Self>) {
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
        let read_source = std::sync::Arc::new(RuntimeReadSource::default());
        let submitted_source = read_source.clone();
        let task = self.runtime.spawn(async move {
            ObservedPage::load_with_source(session, page, &submitted_source).await
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
                        if this.shell.startup_error.is_none() {
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

    pub(crate) fn publish_page_refresh_error(
        &mut self,
        source: &RuntimeReadSource,
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

    pub(in crate::features) fn accept_page_observation(
        &mut self,
        observed: ObservedPage,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut data_accepted = false;
        let accepted = observed.publish(|result, runtime| {
            let data = match result {
                Ok(data) => data,
                Err(error) => {
                    if self.shell.startup_error.is_none() {
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

    pub(in crate::features) fn update_observed_page_presentation(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if let PageSnapshotData::Resources { proxy, rules, .. } = self.page_observation.data()
            && let Some(identity) = self.page_observation.source().as_ref()
        {
            self.monitors
                .provider_operations
                .observe_catalog_for_runtime(ProviderKind::Proxy, proxy, identity);
            self.monitors
                .provider_operations
                .observe_catalog_for_runtime(ProviderKind::Rule, rules, identity);
        }
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.update_rule_presentation(cx);
    }

    pub(in crate::features) fn apply_controlled_config(
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
        let commands = self.application_commands.clone();
        let task = self.runtime.spawn(async move {
            let outcome = commands
                .apply_controlled_patch(&controlled, profile, patch)
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
                            this.monitors.log_monitor.set_level(level);
                        }
                        this.reload_controlled_config(cx);
                        this.library.overrides.invalidate_preview();
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

    pub(in crate::features) fn controlled_bool(&self, pointer: &str, fallback: bool) -> bool {
        self.config_inputs
            .effective()
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    }

    pub(crate) fn reload_controlled_config(&mut self, cx: &mut Context<Self>) {
        self.settings.controlled_config_generation =
            self.settings.controlled_config_generation.wrapping_add(1);
        let generation = self.settings.controlled_config_generation;
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
                if this.settings.controlled_config_generation != generation {
                    return;
                }
                match result {
                    Ok(controlled) => {
                        this.settings.controlled_config = controlled;
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

    pub(in crate::features) fn core_busy(&self) -> bool {
        self.mutation_busy(MutationDomain::Core)
    }

    pub(in crate::features) fn mutation_busy(&self, domain: MutationDomain) -> bool {
        self.persistent_loading || self.mutations.busy(domain)
    }

    pub(in crate::features) fn begin_mutation(&mut self, page: Page) -> Option<PageTaskToken> {
        self.begin_scoped_mutation(page, MutationDomain::Core)
    }

    pub(in crate::features) fn begin_scoped_mutation(
        &mut self,
        page: Page,
        domain: MutationDomain,
    ) -> Option<PageTaskToken> {
        if self.persistent_loading {
            return None;
        }
        let mutation = self.mutations.begin(domain)?;
        self.page_observation.clear_cache();
        if matches!(
            domain,
            MutationDomain::Core | MutationDomain::Backup | MutationDomain::BackupRecovery
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

    pub(in crate::features) fn finish_mutation(&mut self, token: PageTaskToken) {
        self.synchronize_profile_recovery();
        if let Some(mutation) = token.mutation {
            self.mutations.finish(mutation);
        }
    }

    pub(in crate::features) fn invalidate_page_load(&mut self) {
        self.page_observation.cancel();
        self.loading = false;
    }

    pub(in crate::features) const fn page_task_token_for(&self, page: Page) -> PageTaskToken {
        PageTaskToken {
            page,
            navigation_generation: self.navigation_generation,
            mutation: None,
        }
    }

    pub(in crate::features) fn is_page_task_current(&self, token: PageTaskToken) -> bool {
        token.is_current(self.page, self.navigation_generation)
    }

    pub(in crate::features) fn replace_page_data(
        &mut self,
        token: PageTaskToken,
        data: PageSnapshotData,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_page_task_current(token) {
            return false;
        }
        if let PageSnapshotData::Resources { proxy, rules, .. } = &data {
            self.monitors
                .provider_operations
                .observe_catalog(ProviderKind::Proxy, proxy);
            self.monitors
                .provider_operations
                .observe_catalog(ProviderKind::Rule, rules);
        }
        self.page_observation
            .replace_unobserved(data, self.core_session.generation());
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
        self.update_rule_presentation(cx);
        true
    }

    pub(in crate::features) fn set_page_error(&mut self, token: PageTaskToken, error: String) {
        if self.is_page_task_current(token) {
            self.error = Some(error);
        } else {
            tracing::warn!(page = ?token.page, %error, "discarded error from an inactive page task");
        }
    }

    pub(in crate::features) fn config(&self) -> Option<&RuntimeConfig> {
        self.page_observation.config(self.core_session.generation())
    }

    pub(in crate::features) fn mihomo_binary(&self) -> Option<std::path::PathBuf> {
        self.core_session
            .runtime_descriptor()
            .binary()
            .map(std::path::Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui_kit::test]
    fn successful_bootstrap_preserves_an_earlier_page_read_error(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use crate::test_support::{RuntimePageFixture, open_runtime_page_window};

        let fixture = RuntimePageFixture::new();
        let (window, page) = open_runtime_page_window(cx, &fixture, Page::Override);
        fixture.wait_for_initial_reads(cx, &page);
        let state = load_initial_persistent_state(
            &fixture.controlled,
            Some(fixture.profiles.clone()),
            Some(fixture.overrides.clone()),
        );
        assert!(state.error.is_none());
        let task = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move { state });
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                // Force the failing page read to publish before successful bootstrap adoption.
                page.error = Some("resources query failed".into());
                page.persistent_loading = true;
                page.config_inputs.begin_bootstrap_read();
                page.finish_initial_persistent_state(task, fixture.core.generation(), cx);
            });
        });
        fixture.wait_for_initial_reads(cx, &page);
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                page.read(cx).error.as_deref(),
                Some("resources query failed")
            );
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn report_automatic_runtime_preserves_error_when_notice_triggers_refresh(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use crate::test_support::{RuntimePageFixture, open_runtime_page_window};

        let fixture = RuntimePageFixture::new();
        let (window, page) = open_runtime_page_window(cx, &fixture, Page::Home);
        fixture.wait_for_initial_reads(cx, &page);
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                page.report_automatic_runtime(
                    Some("automatic.network_stopped"),
                    Some("network probe failed".into()),
                    cx,
                );
            });
            assert_eq!(
                page.read(cx).error.as_deref(),
                Some(
                    zenclash_i18n::text_with(
                        "automatic.failed",
                        &[("error", "network probe failed".to_owned())],
                    )
                    .as_str()
                )
            );
            assert_eq!(
                page.read(cx).notice.as_deref(),
                Some(zenclash_i18n::text("automatic.network_stopped").as_str())
            );
            window.remove_window();
        })
        .unwrap();
    }
}

#[cfg(test)]
mod ui_tests;
