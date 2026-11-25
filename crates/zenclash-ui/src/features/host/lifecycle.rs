use gpui_kit::{AppContext, Context, Window};
use serde_json::Value;
use zenclash_core::{
    ControlledConfigStore, ProfileCatalog, ProfileStore, ProviderOperations, YamlOverrideCatalog,
    YamlOverrideStore,
};

use crate::features::host::{
    LibraryPagesState, RuntimePage, RuntimePageServices, SettingsWorkspaceState,
    ShellPresentationState,
};
use crate::foundation::actions::Page;
use crate::runtime_host::{
    busy::MutationState,
    config_inputs::{ConfigInputs, config_input_snapshot},
    lifecycle_schedule::UiVisibility,
    observation::PageObservationOwner,
    services::RuntimeMonitors,
};

mod config_apply;
mod live_loop;
mod page_loading;

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
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_tests;
