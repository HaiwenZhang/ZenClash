use std::{path::PathBuf, sync::Arc};

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ThemeMode, WindowExt};
use gpui_kit::{AppContext, Context, Entity, Subscription, Window};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AppearancePreference, ControlledConfigStore, CoreKind,
    CoreSession, LogMonitor, OperationalStatus, ServiceManager, TrafficCaptureSession,
};

pub use crate::foundation::actions::*;
use crate::outbound_mode::{OutboundMode, OutboundModeCoordinator};
pub use crate::shell::bootstrap::{
    create_main_window, create_main_window_with_pending_startup,
    create_main_window_with_service_startup, init, reopen_main_window,
};
pub use crate::shell::controllers::system_proxy::finish_native_shutdown;
pub use zenclash_app::{
    TrafficHistorySession,
    bootstrap::{AppServices, BootstrappedApplication, RestartRequest},
};

use crate::{
    features::{host::RuntimePage, proxies::ProxiesPage},
    foundation::design::apply_zen_theme,
    runtime_host::services::{
        ContinueLocalRequested, PreferencesRestored, ProfileActivated, ProfileCleared,
        ProxySelectionChanged, RuntimeConfigApplied, RuntimePageServices,
        ServiceRepairRestartRequested,
    },
    shell::tray::native::NetworkTrayIcon,
};

pub(in crate::shell) struct AppCoreHandles {
    pub(in crate::shell) core_kind: CoreKind,
    pub(in crate::shell) core_session: CoreSession,
    pub(in crate::shell) application_commands: crate::ApplicationCommands,
    pub(in crate::shell) runtime: tokio::runtime::Handle,
    pub(in crate::shell) profile_store: Option<zenclash_core::ProfileStore>,
    pub(in crate::shell) override_store: Option<zenclash_core::YamlOverrideStore>,
    pub(in crate::shell) profile_path: Option<PathBuf>,
    pub(in crate::shell) controlled_config_store: ControlledConfigStore,
    pub(in crate::shell) operational_status: Arc<OperationalStatus>,
    pub(in crate::shell) traffic_capture: TrafficCaptureSession,
    pub(in crate::shell) log_monitor: Arc<LogMonitor>,
    pub(in crate::shell) traffic_history_session: Option<Arc<TrafficHistorySession>>,
}

struct WiredRuntime {
    handles: AppCoreHandles,
    pub(in crate::shell) proxies_page: Entity<ProxiesPage>,
    pub(crate) runtime_page: Entity<RuntimePage>,
    subscriptions: Vec<Subscription>,
    initializing: bool,
    await_service_handoff: bool,
    restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    safe_mode: zenclash_app::SafeMode,
}

/// Root GPUI entity coordinating pages, windows, Mihomo state, and native tray.
pub struct ZenClashApp {
    pub(in crate::shell) current_page: Page,
    pub(in crate::shell) sidebar_collapsed: bool,
    pub(in crate::shell) outbound_mode: OutboundModeCoordinator,
    pub(in crate::shell) mode_error: Option<String>,
    pub(in crate::shell) controller_indicator: crate::shell::view::ControllerIndicator,
    pub(in crate::shell) focus_handle: gpui_kit::FocusHandle,
    pub(in crate::shell) proxies_page: Entity<ProxiesPage>,
    pub(crate) runtime_page: Entity<RuntimePage>,
    pub(in crate::shell) shell: crate::shell::window::WindowShellState,
    pub(in crate::shell) tray: crate::shell::tray::TrayController,
    pub(in crate::shell) preferences: crate::shell::controllers::preferences::PreferenceController,
    pub(in crate::shell) services: AppCoreHandles,
    pub(in crate::shell) _subscriptions: Vec<Subscription>,
}

impl ZenClashApp {
    fn wire_runtime(
        services: AppServices,
        initialization: Option<zenclash_core::CoreInitializationOutcome>,
        preferences_store: Option<AppPreferencesStore>,
        preferences: AppPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WiredRuntime {
        let AppServices {
            initializing,
            await_service_handoff,
            profile_store,
            override_store,
            preferences_store: _,
            preferences: _,
            core_kind,
            core_session,
            traffic_capture,
            system_proxy_session,
            client,
            traffic_monitor,
            log_monitor,
            traffic_history_store,
            traffic_history_session,
            profile_path,
            controlled_config_store,
            runtime,
            startup_notice,
            startup_error,
            restart_after_exit,
            safe_mode,
        } = services;
        let proxies_page =
            cx.new(|cx| ProxiesPage::new_with_session(core_session.clone(), runtime.clone(), cx));
        let app_profile_store = profile_store.clone();
        let app_override_store = override_store.clone();
        let app_profile_path = profile_path.clone();
        let app_controlled_config_store = controlled_config_store.clone();
        if !initializing {
            crate::shell::controllers::system_proxy::observe_native_quit(
                &core_session,
                &traffic_capture,
                traffic_history_session.as_ref(),
                &runtime,
                cx,
            );
        }
        let _supervisor_started = !initializing
            && core_session.start_supervisor_with_capture(&runtime, traffic_capture.clone());
        let operational_status = OperationalStatus::start(
            &runtime,
            core_session.clone(),
            system_proxy_session.clone(),
            traffic_monitor.clone(),
            log_monitor.clone(),
        );
        let service_manager = ServiceManager::new(core_session.clone(), traffic_capture.clone());
        if core_session.run_state().health == zenclash_core::ServiceHealthKind::Unknown {
            let service_health = service_manager.clone();
            runtime.spawn(async move {
                let _ = service_health.refresh_health().await;
            });
        }
        let application_commands =
            crate::ApplicationCommands::new(core_session.clone(), override_store.clone())
                .with_service_manager(service_manager);
        if let Some(initialization) = initialization.as_ref() {
            application_commands.record_service_initialization(initialization);
        }
        let runtime_page = cx.new(|cx| {
            RuntimePage::new(
                Page::Home,
                RuntimePageServices {
                    profile_store,
                    override_store,
                    core_kind,
                    core_session: core_session.clone(),
                    profile_service: application_commands.clone(),
                    client: client.clone(),
                    runtime: runtime.clone(),
                    traffic_monitor: traffic_monitor.clone(),
                    log_monitor: log_monitor.clone(),
                    operational_status: operational_status.clone(),
                    traffic_capture: traffic_capture.clone(),
                    profile_path,
                    controlled_config_store,
                    preferences_store,
                    preferences,
                    system_proxy_session,
                    traffic_history_store,
                    startup_notice,
                    startup_error,
                },
                window,
                cx,
            )
        });
        let subscriptions = vec![
            Self::subscribe_profile_events(&runtime_page, cx),
            Self::subscribe_profile_clear_events(&runtime_page, cx),
            Self::subscribe_proxy_selection_events(&runtime_page, cx),
            Self::subscribe_runtime_config_events(&runtime_page, cx),
            Self::subscribe_preference_events(&runtime_page, cx),
            cx.subscribe(&runtime_page, |this, _, _: &ContinueLocalRequested, cx| {
                match std::env::current_exe() {
                    Ok(executable) => this.begin_quit_mode(Some(executable), true, cx),
                    Err(error) => this.runtime_page.update(cx, |page, cx| {
                        page.report_system_proxy_reconcile_error(&error.to_string(), cx)
                    }),
                }
            }),
            cx.subscribe(
                &runtime_page,
                |this, _, _: &ServiceRepairRestartRequested, cx| match std::env::current_exe() {
                    Ok(executable) => this.begin_quit(Some(executable), cx),
                    Err(error) => this.runtime_page.update(cx, |page, cx| {
                        page.report_system_proxy_reconcile_error(&error.to_string(), cx)
                    }),
                },
            ),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.preferences.current.appearance == AppearancePreference::System {
                    apply_zen_theme(ThemeMode::from(window.appearance()), Some(window), cx);
                    cx.notify();
                }
            }),
        ];
        #[cfg(target_os = "linux")]
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && !this.shell.main_window_visible {
                this.restore_main_window_after_activation(cx);
            }
        }));
        WiredRuntime {
            handles: AppCoreHandles {
                core_kind,
                core_session,
                application_commands,
                runtime,
                profile_store: app_profile_store,
                override_store: app_override_store,
                profile_path: app_profile_path,
                controlled_config_store: app_controlled_config_store,
                operational_status,
                traffic_capture,
                log_monitor,
                traffic_history_session,
            },
            proxies_page,
            runtime_page,
            subscriptions,
            initializing,
            await_service_handoff,
            restart_after_exit,
            safe_mode,
        }
    }

    pub(crate) fn new(
        services: AppServices,
        initialization: Option<zenclash_core::CoreInitializationOutcome>,
        network_tray: Option<NetworkTrayIcon>,
        preferences_store: Option<AppPreferencesStore>,
        preferences: AppPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let main_window = window.window_handle();
        let wired = Self::wire_runtime(
            services,
            initialization,
            preferences_store.clone(),
            preferences.clone(),
            window,
            cx,
        );
        let mut app = Self {
            current_page: Page::default(),
            sidebar_collapsed: false,
            outbound_mode: OutboundModeCoordinator::new_unsynchronized(OutboundMode::default()),
            mode_error: None,
            controller_indicator: crate::shell::view::ControllerIndicator::default(),
            focus_handle,
            proxies_page: wired.proxies_page,
            runtime_page: wired.runtime_page,
            shell: crate::shell::window::WindowShellState::new(
                main_window,
                wired.restart_after_exit,
                wired.safe_mode,
            ),
            tray: crate::shell::tray::TrayController::new(network_tray),
            preferences: crate::shell::controllers::preferences::PreferenceController::new(
                preferences_store,
                preferences,
            ),
            services: wired.handles,
            _subscriptions: wired.subscriptions,
        };
        if !wired.initializing {
            app.start_live_services(wired.await_service_handoff, cx);
        }
        app
    }

    fn start_live_services(&mut self, await_service_handoff: bool, cx: &mut Context<Self>) {
        self.start_runtime_updates(cx);
        self.restore_system_proxy(cx);
        self.start_mode_sync(cx);
        self.start_profile_updates(cx);
        self.start_automatic_runtime(cx);
        if await_service_handoff {
            self.start_service_handoff(cx);
        }
        self.start_tray_updates(cx);
        self.refresh_tray_menu(cx);
    }

    pub(in crate::shell) fn finish_startup(
        &mut self,
        result: Result<BootstrappedApplication, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shell.quit_state != crate::shell::controllers::system_proxy::QuitState::Idle {
            return;
        }
        let BootstrappedApplication {
            mut services,
            initialization,
        } = match result {
            Ok(application) => application,
            Err(error) => {
                self.runtime_page.update(cx, |page, cx| {
                    page.report_system_proxy_reconcile_error(&error, cx)
                });
                return;
            }
        };
        // Preferences may have changed while service startup was pending.
        if let Some(store) = services.preferences_store.as_ref()
            && let Ok(preferences) = store.load()
        {
            services.preferences = preferences;
        }
        self.preferences
            .menu_saves
            .previews()
            .apply(&mut services.preferences);
        let preferences = services.preferences.clone();
        let preferences_store = services.preferences_store.clone();
        let network_tray = NetworkTrayIcon::new()
            .inspect_err(
                |error| tracing::warn!(%error, "failed to create native traffic tray icon"),
            )
            .ok();
        if let Some(tray) = network_tray.as_ref()
            && let Err(error) = tray.set_visible(preferences.traffic_tray_visible)
        {
            tracing::warn!(%error, "failed to restore traffic tray visibility");
        }
        if let Err(error) = crate::shell::bootstrap::configure_log_monitor(
            &services.log_monitor,
            preferences_store.as_ref(),
            &preferences,
        ) {
            tracing::warn!(%error, "failed to configure core log persistence after startup");
        }
        self.adopt_startup(
            BootstrappedApplication {
                services,
                initialization,
            },
            network_tray,
            window,
            cx,
        );
    }

    pub(in crate::shell) fn adopt_startup(
        &mut self,
        application: BootstrappedApplication,
        network_tray: Option<NetworkTrayIcon>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let BootstrappedApplication {
            mut services,
            initialization,
        } = application;
        let previews = self.preferences.menu_saves.previews();
        previews.apply(&mut services.preferences);
        let preferences = services.preferences.clone();
        let preferences_store = services.preferences_store.clone();
        let page = self.current_page;
        let window_visible = self.shell.main_window_visible;

        // Cancel any previously owned shell tasks before wiring live runtime services in place.
        self.shell.tasks.clear();
        let wired = Self::wire_runtime(
            services,
            initialization,
            preferences_store.clone(),
            preferences.clone(),
            window,
            cx,
        );
        self.outbound_mode = OutboundModeCoordinator::new_unsynchronized(OutboundMode::default());
        self.mode_error = None;
        self.controller_indicator = crate::shell::view::ControllerIndicator::default();
        self.proxies_page = wired.proxies_page;
        self.runtime_page = wired.runtime_page;
        self.services = wired.handles;
        self.shell.restart_after_exit = wired.restart_after_exit;
        self.shell.safe_mode = wired.safe_mode;
        self.tray = crate::shell::tray::TrayController::new(network_tray);
        self.preferences.store = preferences_store;
        self.preferences.current = preferences;
        self._subscriptions = wired.subscriptions;
        if !wired.initializing {
            self.start_live_services(wired.await_service_handoff, cx);
        }
        self.runtime_page
            .update(cx, |page, cx| page.set_preference_previews(previews, cx));
        self.navigate(page, cx);
        if !window_visible {
            self.release_hidden_page_data(cx);
        }
        cx.notify();
    }

    fn subscribe_profile_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, event: &ProfileActivated, cx| {
            let snapshot = this.services.application_commands.committed_profile();
            let path = if snapshot.generation == event.runtime_version {
                event.path.clone()
            } else if let Some(path) = snapshot.profile_path {
                path
            } else {
                return;
            };
            this.apply_profile_change(Some(path), cx);
        })
    }

    fn subscribe_profile_clear_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, event: &ProfileCleared, cx| {
            let snapshot = this.services.application_commands.committed_profile();
            let path = if snapshot.generation == event.runtime_version {
                None
            } else {
                snapshot.profile_path
            };
            this.apply_profile_change(path, cx);
        })
    }

    pub(in crate::shell) fn apply_profile_change(
        &mut self,
        path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.services.profile_path = path.clone();
        self.services.traffic_capture.set_profile(path.clone());
        if path.is_some() && self.current_page == Page::Proxies && self.shell.main_window_visible {
            self.proxies_page
                .update(cx, crate::features::proxies::ProxiesPage::profile_activated);
        } else {
            self.proxies_page
                .update(cx, |page, _| page.profile_invalidated());
        }
        self.restore_system_proxy(cx);
        self.refresh_tray_menu(cx);
    }

    fn subscribe_proxy_selection_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, _: &ProxySelectionChanged, cx| {
            this.refresh_visible_proxies(cx);
            this.refresh_tray_menu(cx);
        })
    }

    fn subscribe_runtime_config_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, _: &RuntimeConfigApplied, cx| {
            this.restore_system_proxy(cx);
            this.refresh_tray_menu(cx);
        })
    }

    fn subscribe_preference_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(
            runtime_page,
            move |this, _, event: &PreferencesRestored, cx| {
                this.accept_preferences_event(event, cx);
            },
        )
    }

    fn start_runtime_updates(&mut self, cx: &mut Context<Self>) {
        let mode = self.outbound_mode.clone();
        let mut mode_updates = mode.subscribe();
        let core = self.services.core_session.clone();
        let mut operational_updates = self.services.operational_status.subscribe();
        let mut run_state_updates = core.subscribe_run_state();
        let mut observed_process_running = None;
        let mut initialize = true;
        let task = cx.spawn(async move |this, cx| {
            loop {
                let (mut mode_changed, mut process_changed) = (initialize, initialize);
                let mut run_state_changed = initialize;
                if initialize {
                    initialize = false;
                } else {
                    tokio::select! {
                        result = mode_updates.changed() => {
                            if result.is_err() {
                                break;
                            }
                            mode_changed = true;
                        }
                        result = run_state_updates.changed() => {
                            if result.is_err() { break; }
                            run_state_changed = true;
                        }
                        result = operational_updates.changed() => {
                            if result.is_err() {
                                break;
                            }
                            process_changed = true;
                        }
                    }
                }

                run_state_changed |= run_state_updates.has_changed().unwrap_or(false);
                if run_state_changed {
                    run_state_updates.borrow_and_update();
                }
                mode_changed |= mode_updates.has_changed().unwrap_or(false);
                process_changed |= operational_updates.has_changed().unwrap_or(false);
                if mode_changed {
                    mode_updates.borrow_and_update();
                }
                let controller_indicator = process_changed.then(|| {
                    let snapshot = operational_updates.borrow();
                    crate::shell::view::ControllerIndicator::from_observation(
                        &snapshot.controller,
                        core.generation(),
                    )
                });
                let process_running = process_changed.then(|| {
                    let snapshot = operational_updates.borrow_and_update();
                    snapshot
                        .process
                        .is_fresh()
                        .then(|| snapshot.process.value())
                        .flatten()
                        .filter(|process| process.generation == core.generation())
                        .map(|process| process.running)
                });
                if this
                    .update(cx, |this, cx| {
                        let mut refresh_tray = false;
                        if run_state_changed {
                            this.runtime_page.update(cx, |_, cx| cx.notify());
                            cx.notify();
                        }
                        if let Some(indicator) = controller_indicator
                            && indicator != this.controller_indicator
                        {
                            this.controller_indicator = indicator;
                            cx.notify();
                        }
                        if mode_changed {
                            let displayed = mode.displayed();
                            let pending = mode.is_pending();
                            this.proxies_page.update(cx, |page, cx| {
                                page.synchronize_outbound_mode(displayed.api_value(), pending, cx);
                            });
                            let error = mode.error();
                            if error != this.mode_error {
                                this.mode_error = error.clone();
                                if let Some(error) = error {
                                    let _ = cx.update_window(
                                        this.shell.main_window,
                                        |_, window, cx| {
                                            window
                                                .push_notification(Notification::error(error), cx);
                                        },
                                    );
                                }
                                cx.notify();
                            }
                            this.runtime_page.update(cx, |page, cx| {
                                page.update_home_mode_transition_if_active(displayed, pending, cx);
                            });
                            refresh_tray = true;
                        }
                        if let Some(process_running) = process_running
                            && process_running != observed_process_running
                        {
                            observed_process_running = process_running;
                            refresh_tray = true;
                        }
                        if refresh_tray {
                            this.refresh_tray_menu(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.shell.tasks.push(task);
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ZenClashApp {
    pub(crate) const fn current_page_for_test(&self) -> Page {
        self.current_page
    }

    pub(crate) const fn sidebar_collapsed_for_test(&self) -> bool {
        self.sidebar_collapsed
    }

    pub(crate) fn outbound_mode_for_test(&self) -> OutboundMode {
        self.outbound_mode.displayed()
    }
}
