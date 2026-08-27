use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use gpui::{
    div, px, AnyWindowHandle, App, AppContext, ClipboardItem, Context, Entity, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled,
    Subscription, Window, WindowBounds, WindowKind, WindowOptions,
};
use gpui_component::{h_flex, v_flex, ActiveTheme, Root, ThemeMode, TitleBar};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AppearancePreference, ControlledConfigStore, CoreKind,
    LogMonitor, MihomoClient, MihomoLogLevel, MihomoProcess, SystemProxyController,
    TrafficHistoryStore, TrafficMonitor,
};

mod actions;
mod bootstrap;
mod platform;
mod profile_updates;
mod system_proxy;
mod traffic_history;
mod tray;
mod view;
mod webdav_backups;

pub use bootstrap::{create_main_window, init};
use platform::{open_directory, tray_directories};
use tray::LatestCommandQueue;

use crate::{
    components::{
        floating::FloatingTrafficWindow,
        mode::OutboundModeCoordinator,
        sidebar::{OutboundMode, Sidebar},
        tray::{
            EnvironmentShell, NetworkTrayIcon, TrayClick, TrayCommand, TrayMenuState, TrayProfile,
            TrayProxyGroup, TrayProxyNode,
        },
    },
    design::apply_zen_theme,
    pages::{
        proxies::ProxiesPage,
        runtime::{
            ElevatedRestartRequested, PreferencesRestored, ProfileActivated, RuntimeConfigApplied,
            RuntimePage, RuntimePageServices,
        },
        Page,
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
            ToggleFloatingWindow,
        ]
    );
}

pub use action_types::*;

/// Root GPUI entity coordinating pages, windows, Mihomo state, and native tray.
pub struct ZenClashApp {
    current_page: Page,
    outbound_mode: OutboundModeCoordinator,
    core_kind: CoreKind,
    client: MihomoClient,
    traffic_monitor: Arc<TrafficMonitor>,
    runtime: tokio::runtime::Handle,
    focus_handle: gpui::FocusHandle,
    proxies_page: Entity<ProxiesPage>,
    runtime_page: Entity<RuntimePage>,
    mihomo_process: Option<Arc<MihomoProcess>>,
    managed_core_running: Option<bool>,
    profile_path: Option<PathBuf>,
    controlled_config_store: ControlledConfigStore,
    main_window: AnyWindowHandle,
    floating_window: Option<AnyWindowHandle>,
    tray_refreshing: bool,
    tray_refresh_pending: bool,
    tray_menu_requested: bool,
    system_proxy_commands: LatestCommandQueue<(bool, u16)>,
    system_proxy_controller: SystemProxyController,
    quit_state: system_proxy::QuitState,
    tun_commands: LatestCommandQueue<bool>,
    proxy_selection_commands: LatestCommandQueue<(String, String)>,
    profile_selection_commands: LatestCommandQueue<String>,
    network_tray: Option<NetworkTrayIcon>,
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    restart_after_exit: Arc<parking_lot::Mutex<Option<PathBuf>>>,
    restart_elevated_after_exit: Arc<AtomicBool>,
    log_monitor: Arc<LogMonitor>,
    traffic_history_policy: Arc<traffic_history::TrafficHistoryPolicy>,
    _subscriptions: Vec<Subscription>,
}

/// Runtime services prepared by the executable before constructing the UI.
pub struct AppServices {
    /// Persistent application-preference store discovered during process bootstrap.
    pub preferences_store: Option<AppPreferencesStore>,
    /// Single validated preference snapshot used consistently for startup and UI state.
    pub preferences: AppPreferences,
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Typed external-controller client.
    pub client: MihomoClient,
    /// Shared reconnecting traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared reconnecting log stream.
    pub log_monitor: Arc<LogMonitor>,
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
    /// Whether the deferred restart must use the Windows RunAs verb.
    pub restart_elevated_after_exit: Arc<AtomicBool>,
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
            client,
            traffic_monitor,
            log_monitor,
            mihomo_process,
            profile_path,
            controlled_config_store,
            runtime,
            startup_notice,
            startup_error,
            restart_after_exit,
            restart_elevated_after_exit,
        } = services;
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let main_window = window.window_handle();
        let proxies_page = cx.new(|cx| ProxiesPage::new(client.clone(), runtime.clone(), cx));
        let app_profile_path = profile_path.clone();
        let app_controlled_config_store = controlled_config_store.clone();
        let managed_core_running = mihomo_process.as_ref().map(|process| process.is_running());
        let traffic_history_store = TrafficHistoryStore::discover()
            .inspect_err(
                |error| tracing::warn!(%error, "failed to discover traffic-history database"),
            )
            .ok();
        let traffic_history_policy =
            Arc::new(traffic_history::TrafficHistoryPolicy::new(&preferences));
        let system_proxy_controller = SystemProxyController::default();
        let runtime_page = cx.new(|cx| {
            RuntimePage::new(
                Page::Home,
                RuntimePageServices {
                    core_kind,
                    client: client.clone(),
                    runtime: runtime.clone(),
                    traffic_monitor: traffic_monitor.clone(),
                    log_monitor: log_monitor.clone(),
                    process: mihomo_process.clone(),
                    profile_path,
                    controlled_config_store,
                    preferences_store: preferences_store.clone(),
                    preferences: preferences.clone(),
                    system_proxy_controller: system_proxy_controller.clone(),
                    traffic_history_store: traffic_history_store.clone(),
                    startup_notice,
                    startup_error,
                },
                window,
                cx,
            )
        });
        let profile_subscription = Self::subscribe_profile_events(&runtime_page, cx);
        let runtime_config_subscription = Self::subscribe_runtime_config_events(&runtime_page, cx);
        let preferences_subscription = Self::subscribe_preference_events(&runtime_page, cx);
        let elevated_restart_subscription =
            Self::subscribe_elevated_restart_events(&runtime_page, cx);
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            if this.preferences.appearance == AppearancePreference::System {
                apply_zen_theme(ThemeMode::from(window.appearance()), Some(window), cx);
                cx.notify();
            }
        });
        let mut app = Self {
            current_page: Page::default(),
            outbound_mode: OutboundModeCoordinator::new_unsynchronized(OutboundMode::default()),
            core_kind,
            client,
            traffic_monitor,
            runtime,
            focus_handle,
            proxies_page,
            runtime_page,
            mihomo_process,
            managed_core_running,
            profile_path: app_profile_path,
            controlled_config_store: app_controlled_config_store,
            main_window,
            floating_window: None,
            tray_refreshing: false,
            tray_refresh_pending: false,
            tray_menu_requested: false,
            system_proxy_commands: LatestCommandQueue::default(),
            system_proxy_controller,
            quit_state: system_proxy::QuitState::default(),
            tun_commands: LatestCommandQueue::default(),
            proxy_selection_commands: LatestCommandQueue::default(),
            profile_selection_commands: LatestCommandQueue::default(),
            network_tray,
            preferences_store,
            preferences,
            restart_after_exit,
            restart_elevated_after_exit,
            log_monitor,
            traffic_history_policy,
            _subscriptions: vec![
                profile_subscription,
                runtime_config_subscription,
                preferences_subscription,
                elevated_restart_subscription,
                appearance_subscription,
            ],
        };
        app.start_traffic_updates(cx);
        app.restore_system_proxy(cx);
        app.start_mode_sync(cx);
        app.start_profile_updates(cx);
        app.start_webdav_backups(cx);
        app.start_traffic_history(traffic_history_store);
        Self::start_tray_updates(cx);
        app.refresh_tray_menu(cx);
        app
    }

    fn subscribe_profile_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, event: &ProfileActivated, cx| {
            this.profile_path = Some(event.path.clone());
            this.proxies_page
                .update(cx, super::pages::proxies::ProxiesPage::profile_activated);
            this.restore_system_proxy(cx);
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
        cx.subscribe(runtime_page, |this, _, event: &PreferencesRestored, cx| {
            this.preferences = event.preferences.clone();
            this.traffic_history_policy.update(&this.preferences);
            if let Err(error) = bootstrap::configure_log_monitor(
                &this.log_monitor,
                this.preferences_store.as_ref(),
                &this.preferences,
            ) {
                tracing::warn!(%error, "failed to apply restored log persistence settings");
            }
            if let Some(tray) = &this.network_tray {
                if let Err(error) = tray.set_visible(this.preferences.traffic_tray_visible) {
                    tracing::warn!(%error, "failed to apply restored tray visibility");
                }
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
        })
    }

    fn subscribe_elevated_restart_events(
        runtime_page: &Entity<RuntimePage>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(runtime_page, |this, _, _: &ElevatedRestartRequested, cx| {
            let executable = match std::env::current_exe() {
                Ok(executable) => executable,
                Err(error) => {
                    tracing::warn!(%error, "failed to resolve executable for elevated restart");
                    return;
                }
            };
            this.restart_elevated_after_exit
                .store(true, Ordering::Release);
            this.begin_quit(Some(executable), cx);
        })
    }

    fn start_traffic_updates(&mut self, cx: &mut Context<Self>) {
        let monitor = self.traffic_monitor.clone();
        let mode = self.outbound_mode.clone();
        let mut observed_mode_revision = mode.revision();
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let snapshot = monitor.snapshot();
            let mode_revision = mode.revision();
            if this
                .update(cx, |this, cx| {
                    if observed_mode_revision != mode_revision {
                        observed_mode_revision = mode_revision;
                        let displayed = mode.displayed();
                        this.proxies_page.update(cx, |page, cx| {
                            page.set_outbound_mode(displayed.api_value(), cx);
                        });
                        this.refresh_tray_menu(cx);
                    }
                    let managed_core_running = this
                        .mihomo_process
                        .as_ref()
                        .map(|process| process.is_running());
                    if managed_core_running != this.managed_core_running {
                        this.managed_core_running = managed_core_running;
                        if let Some(running) = managed_core_running {
                            this.runtime_page.update(cx, |page, cx| {
                                page.report_managed_core_state(running, cx);
                            });
                            this.restore_system_proxy(cx);
                            this.refresh_tray_menu(cx);
                        }
                    }
                    if let Some(tray) = this.network_tray.as_mut() {
                        if let Err(error) = tray.update(&snapshot) {
                            tracing::warn!(%error, "failed to update native traffic tray");
                        }
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn start_mode_sync(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let mode = self.outbound_mode.clone();
        let logs = self.log_monitor.clone();
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
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
                Err(error) => tracing::warn!(%error, "outbound mode synchronization task failed"),
            }
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
    }
}
