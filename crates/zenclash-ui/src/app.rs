use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    AnyWindowHandle, App, AppContext, ClipboardItem, Context, Entity, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Pixels, Render, SharedString, Size,
    Styled, Subscription, Window, WindowBounds, WindowKind, WindowOptions, div, px,
};
use gpui_component::{ActiveTheme, Root, ThemeMode, TitleBar, h_flex, v_flex};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AppearancePreference, ControlledConfigStore, CoreKind,
    CoreSession, LogMonitor, MihomoClient, MihomoLogLevel, MihomoProcess, OperationalStatus,
    SystemProxyController, SystemProxySession, TrafficCaptureSession, TrafficHistoryStore,
    TrafficMonitor, TunPermissionManager,
};

mod actions;
mod bootstrap;
pub(crate) mod platform;
mod profile_updates;
mod system_proxy;
mod traffic_history;
mod tray;
mod view;

pub use bootstrap::{create_main_window, init};
use platform::{open_directory, tray_directories};
use tray::LatestCommandQueue;

use crate::{
    components::{
        floating::FloatingTrafficWindow,
        mode::OutboundModeCoordinator,
        sidebar::{OutboundMode, Sidebar},
        tray::{
            EnvironmentShell, NetworkTrayIcon, TrayClick, TrayCommand, TrayEvent, TrayMenuState,
            TrayProfile, TrayProxyGroup, TrayProxyNode,
        },
    },
    design::apply_zen_theme,
    pages::{
        Page,
        proxies::ProxiesPage,
        runtime::{
            PreferencesRestored, ProfileActivated, ProxySelectionChanged, RuntimeConfigApplied,
            RuntimePage, RuntimePageServices,
        },
    },
};

// GPUI's macro offers no hook for documenting its generated marker structs, so
// the exception is confined to this definition-only module.
#[allow(
    clippy::derive_partial_eq_without_eq,
    missing_docs,
    reason = "the gpui actions macro controls generated marker derives and documentation"
)]
mod action_types {
    use gpui::actions;

    actions!(
        zenclash,
        [
            Quit,
            NavigateHome,
            NavigateSystemProxy,
            NavigateTun,
            NavigateProfiles,
            NavigateProxies,
            NavigateMihomo,
            NavigateConnections,
            NavigateDns,
            NavigateSniffer,
            NavigateLogs,
            NavigateRules,
            NavigateResources,
            NavigateOverride,
            NavigateNetwork,
            NavigateTraffic,
            NavigateSettings,
            SetRuleMode,
            SetGlobalMode,
            SetDirectMode,
            SetSystemTheme,
            SetLightTheme,
            SetDarkTheme,
            ShowTrafficIcon,
            HideTrafficIcon,
            ShowStatusMenu,
            ToggleSidebar,
            ToggleFloatingWindow,
        ]
    );
}

pub use action_types::*;

/// Root GPUI entity coordinating pages, windows, Mihomo state, and native tray.
pub struct ZenClashApp {
    current_page: Page,
    sidebar_collapsed: bool,
    outbound_mode: OutboundModeCoordinator,
    core_kind: CoreKind,
    core_session: CoreSession,
    client: MihomoClient,
    traffic_monitor: Arc<TrafficMonitor>,
    runtime: tokio::runtime::Handle,
    focus_handle: gpui::FocusHandle,
    proxies_page: Entity<ProxiesPage>,
    runtime_page: Entity<RuntimePage>,
    tray_core_running: Option<bool>,
    profile_path: Option<PathBuf>,
    controlled_config_store: ControlledConfigStore,
    main_window: AnyWindowHandle,
    main_window_visible: bool,
    main_window_memory: MainWindowMemoryState,
    floating_window: Option<AnyWindowHandle>,
    tray_refreshing: bool,
    tray_refresh_pending: bool,
    tray_menu_requested: bool,
    system_proxy_commands: LatestCommandQueue<(bool, u16)>,
    operational_status: Arc<OperationalStatus>,
    traffic_capture: TrafficCaptureSession,
    quit_state: system_proxy::QuitState,
    tun_commands: LatestCommandQueue<bool>,
    proxy_selection_commands: LatestCommandQueue<(String, String)>,
    profile_selection_commands: LatestCommandQueue<String>,
    network_tray: Option<NetworkTrayIcon>,
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    restart_after_exit: Arc<parking_lot::Mutex<Option<PathBuf>>>,
    log_monitor: Arc<LogMonitor>,
    traffic_history_policy: Arc<traffic_history::TrafficHistoryPolicy>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct MainWindowMemoryState {
    restore_size: Option<Size<Pixels>>,
}

impl MainWindowMemoryState {
    fn park(&mut self, current_size: Size<Pixels>) -> Size<Pixels> {
        let parked_size = gpui::size(px(1.), px(1.));
        if current_size != parked_size && self.restore_size.is_none() {
            self.restore_size = Some(current_size);
        }
        parked_size
    }

    fn restore(&mut self) -> Option<Size<Pixels>> {
        self.restore_size.take()
    }
}

/// Runtime services prepared by the executable before constructing the UI.
pub struct AppServices {
    /// Persistent application-preference store discovered during process bootstrap.
    pub preferences_store: Option<AppPreferencesStore>,
    /// Single validated preference snapshot used consistently for startup and UI state.
    pub preferences: AppPreferences,
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Serialized runtime-core transition owner.
    pub core_session: CoreSession,
    /// Typed external-controller client.
    pub client: MihomoClient,
    /// Shared reconnecting traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared reconnecting log stream.
    pub log_monitor: Arc<LogMonitor>,
    /// Traffic-history store discovered before GPUI starts handling events.
    pub traffic_history_store: Option<TrafficHistoryStore>,
    /// TUN permission manager validated before GPUI starts handling events.
    pub tun_permissions: Option<TunPermissionManager>,
    /// Managed Mihomo child, absent when `ZenClash` attaches to an external core.
    pub mihomo_process: Option<Arc<MihomoProcess>>,
    /// Active Clash/Mihomo YAML path, when known.
    pub profile_path: Option<PathBuf>,
    /// Persistent controlled-config layer merged over the active profile.
    pub controlled_config_store: ControlledConfigStore,
    /// Tokio runtime used for network and blocking bridge tasks.
    pub runtime: tokio::runtime::Handle,
    /// Visible explanation when startup recovered from an unusable preferred core.
    pub startup_notice: Option<String>,
    /// Persistent startup failure shown while the app runs in offline recovery mode.
    pub startup_error: Option<String>,
    /// Deferred application restart request consumed after the instance lock is released.
    pub restart_after_exit: Arc<parking_lot::Mutex<Option<PathBuf>>>,
}

impl ZenClashApp {
    fn new(
        services: AppServices,
        network_tray: Option<NetworkTrayIcon>,
        preferences_store: Option<AppPreferencesStore>,
        preferences: AppPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let AppServices {
            preferences_store: _,
            preferences: _,
            core_kind,
            core_session,
            client,
            traffic_monitor,
            log_monitor,
            traffic_history_store,
            tun_permissions,
            mihomo_process,
            profile_path,
            controlled_config_store,
            runtime,
            startup_notice,
            startup_error,
            restart_after_exit,
        } = services;
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let main_window = window.window_handle();
        let proxies_page = cx.new(|cx| ProxiesPage::new(client.clone(), runtime.clone(), cx));
        let app_profile_path = profile_path.clone();
        let app_controlled_config_store = controlled_config_store.clone();
        let traffic_history_policy =
            Arc::new(traffic_history::TrafficHistoryPolicy::new(&preferences));
        let system_proxy_controller = SystemProxyController::default();
        let system_proxy_session = preferences_store
            .clone()
            .map(|store| SystemProxySession::new(store, system_proxy_controller.clone()));
        let traffic_capture = TrafficCaptureSession::new(
            core_session.clone(),
            controlled_config_store.clone(),
            system_proxy_session.clone(),
            tun_permissions.clone(),
            profile_path.clone(),
        );
        let _supervisor_started =
            core_session.start_supervisor_with_capture(&runtime, traffic_capture.clone());
        let operational_status = OperationalStatus::start(
            &runtime,
            core_session.clone(),
            system_proxy_session.clone(),
            tun_permissions,
            traffic_monitor.clone(),
            log_monitor.clone(),
        );
        let runtime_page = cx.new(|cx| {
            RuntimePage::new(
                Page::Home,
                RuntimePageServices {
                    core_kind,
                    core_session: core_session.clone(),
                    client: client.clone(),
                    runtime: runtime.clone(),
                    traffic_monitor: traffic_monitor.clone(),
                    log_monitor: log_monitor.clone(),
                    operational_status: operational_status.clone(),
                    traffic_capture: traffic_capture.clone(),
                    process: mihomo_process.clone(),
                    profile_path,
                    controlled_config_store,
                    preferences_store: preferences_store.clone(),
                    preferences: preferences.clone(),
                    system_proxy_session,
                    traffic_history_store: traffic_history_store.clone(),
                    startup_notice,
                    startup_error,
                },
                window,
                cx,
            )
        });
        let profile_subscription = Self::subscribe_profile_events(&runtime_page, cx);
        let proxy_selection_subscription =
            Self::subscribe_proxy_selection_events(&runtime_page, cx);
        let runtime_config_subscription = Self::subscribe_runtime_config_events(&runtime_page, cx);
        let preferences_subscription = Self::subscribe_preference_events(&runtime_page, cx);
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            if this.preferences.appearance == AppearancePreference::System {
                apply_zen_theme(ThemeMode::from(window.appearance()), Some(window), cx);
                cx.notify();
            }
        });
        let mut app = Self {
            current_page: Page::default(),
            sidebar_collapsed: false,
            outbound_mode: OutboundModeCoordinator::new_unsynchronized(OutboundMode::default()),
            core_kind,
            core_session,
            client,
            traffic_monitor,
            runtime,
            focus_handle,
            proxies_page,
            runtime_page,
            tray_core_running: None,
            profile_path: app_profile_path,
            controlled_config_store: app_controlled_config_store,
            main_window,
            main_window_visible: true,
            main_window_memory: MainWindowMemoryState::default(),
            floating_window: None,
            tray_refreshing: false,
            tray_refresh_pending: false,
            tray_menu_requested: false,
            system_proxy_commands: LatestCommandQueue::default(),
            operational_status,
            traffic_capture,
            quit_state: system_proxy::QuitState::default(),
            tun_commands: LatestCommandQueue::default(),
            proxy_selection_commands: LatestCommandQueue::default(),
            profile_selection_commands: LatestCommandQueue::default(),
            network_tray,
            preferences_store,
            preferences,
            restart_after_exit,
            log_monitor,
            traffic_history_policy,
            _subscriptions: vec![
                profile_subscription,
                proxy_selection_subscription,
                runtime_config_subscription,
                preferences_subscription,
                appearance_subscription,
            ],
        };
        app.start_traffic_updates(cx);
        app.restore_system_proxy(cx);
        app.start_mode_sync(cx);
        app.start_profile_updates(cx);
        app.start_traffic_history(traffic_history_store);
        app.start_tray_updates(cx);
        app.refresh_tray_menu(cx);
        app
    }

    fn subscribe_profile_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, event: &ProfileActivated, cx| {
            this.profile_path = Some(event.path.clone());
            this.traffic_capture.set_profile(Some(event.path.clone()));
            if this.current_page == Page::Proxies && this.main_window_visible {
                this.proxies_page
                    .update(cx, super::pages::proxies::ProxiesPage::profile_activated);
            } else {
                this.proxies_page
                    .update(cx, |page, _| page.profile_invalidated());
            }
            this.restore_system_proxy(cx);
            this.refresh_tray_menu(cx);
        })
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
        let runtime_page_for_localization = runtime_page.clone();
        cx.subscribe(
            runtime_page,
            move |this, _, event: &PreferencesRestored, cx| {
                let locale_changed = this.preferences.language != event.preferences.language;
                this.preferences = event.preferences.clone();
                if locale_changed {
                    zenclash_i18n::set_locale(this.preferences.language.locale());
                    bootstrap::refresh_native_app_menu(cx);
                    this.proxies_page.update(cx, |_, cx| cx.notify());
                    let runtime_page = runtime_page_for_localization.clone();
                    let main_window = this.main_window;
                    cx.defer(move |cx| {
                        let _ = cx.update_window(main_window, |_, window, cx| {
                            runtime_page.update(cx, |page, cx| {
                                page.refresh_localized_placeholders(window, cx);
                            });
                        });
                    });
                }
                this.traffic_history_policy.update(&this.preferences);
                if let Err(error) = bootstrap::configure_log_monitor(
                    &this.log_monitor,
                    this.preferences_store.as_ref(),
                    &this.preferences,
                ) {
                    tracing::warn!(%error, "failed to apply restored log persistence settings");
                }
                if let Some(tray) = &this.network_tray
                    && let Err(error) = tray.set_visible(this.preferences.traffic_tray_visible)
                {
                    tracing::warn!(%error, "failed to apply restored tray visibility");
                }
                let appearance = this.preferences.appearance;
                let _ = cx.update_window(this.main_window, move |_, window, cx| {
                    let mode = match appearance {
                        AppearancePreference::System => ThemeMode::from(window.appearance()),
                        AppearancePreference::Light => ThemeMode::Light,
                        AppearancePreference::Dark => ThemeMode::Dark,
                    };
                    apply_zen_theme(mode, Some(window), cx);
                });
                this.refresh_tray_menu(cx);
                cx.notify();
            },
        )
    }

    fn start_traffic_updates(&mut self, cx: &mut Context<Self>) {
        let monitor = self.traffic_monitor.clone();
        let mode = self.outbound_mode.clone();
        let mut traffic_updates = monitor.subscribe();
        let mut mode_updates = mode.subscribe();
        let mut operational_updates = self.operational_status.subscribe();
        let mut observed_process_running = None;
        let mut initialize = true;
        cx.spawn(async move |this, cx| {
            loop {
                let (mut traffic_changed, mut mode_changed, mut process_changed) =
                    (initialize, initialize, initialize);
                if initialize {
                    initialize = false;
                } else {
                    tokio::select! {
                        result = traffic_updates.changed() => {
                            if result.is_err() {
                                break;
                            }
                            traffic_changed = true;
                        }
                        result = mode_updates.changed() => {
                            if result.is_err() {
                                break;
                            }
                            mode_changed = true;
                        }
                        result = operational_updates.changed() => {
                            if result.is_err() {
                                break;
                            }
                            process_changed = true;
                        }
                    }
                }

                traffic_changed |= traffic_updates.has_changed().unwrap_or(false);
                mode_changed |= mode_updates.has_changed().unwrap_or(false);
                process_changed |= operational_updates.has_changed().unwrap_or(false);
                if traffic_changed {
                    traffic_updates.borrow_and_update();
                }
                if mode_changed {
                    mode_updates.borrow_and_update();
                }
                let traffic = traffic_changed.then(|| monitor.snapshot());
                let process_running = process_changed.then(|| {
                    operational_updates
                        .borrow_and_update()
                        .process
                        .value()
                        .map(|process| process.running)
                });
                if this
                    .update(cx, |this, cx| {
                        let mut refresh_tray = false;
                        if mode_changed {
                            let displayed = mode.displayed();
                            this.proxies_page.update(cx, |page, cx| {
                                page.set_outbound_mode(displayed.api_value(), cx);
                            });
                            let pending = mode.is_pending();
                            this.runtime_page.update(cx, |page, cx| {
                                page.update_home_mode_transition_if_active(displayed, pending, cx);
                            });
                            refresh_tray = true;
                        }
                        if let Some(process_running) = process_running
                            && process_running != observed_process_running
                        {
                            observed_process_running = process_running;
                            this.tray_core_running = process_running;
                            refresh_tray = true;
                        }
                        if refresh_tray {
                            this.refresh_tray_menu(cx);
                        }
                        if let Some(traffic) = traffic
                            && let Some(tray) = this.network_tray.as_mut()
                            && let Err(error) = tray.update(&traffic)
                        {
                            tracing::warn!(%error, "failed to update native traffic tray");
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

    fn start_mode_sync(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let mode = self.outbound_mode.clone();
        let logs = self.log_monitor.clone();
        cx.spawn(async move |this, _cx| {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let generation = mode.generation();
                let client = client.clone();
                let task = runtime.spawn(async move { client.runtime_config().await });
                match task.await {
                    Ok(Ok(config)) => {
                        mode.synchronize(OutboundMode::from_api(&config.mode), generation);
                        if let Some(level) = MihomoLogLevel::from_api(&config.log_level) {
                            logs.set_level(level);
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "failed to synchronize Mihomo outbound mode");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "outbound mode synchronization task failed")
                    }
                }
                if this.upgrade().is_none() {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn parked_main_window_restores_its_previous_size() {
        let mut state = MainWindowMemoryState::default();
        let visible_size = gpui::size(px(1280.), px(820.));

        assert_eq!(state.park(visible_size), gpui::size(px(1.), px(1.)));
        assert_eq!(state.restore(), Some(visible_size));
        assert_eq!(state.restore(), None);
    }

    #[test]
    fn repeated_parking_does_not_forget_the_visible_size() {
        let mut state = MainWindowMemoryState::default();
        let visible_size = gpui::size(px(960.), px(640.));

        state.park(visible_size);
        state.park(gpui::size(px(1.), px(1.)));

        assert_eq!(state.restore(), Some(visible_size));
    }
}
