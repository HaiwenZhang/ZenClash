use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    div, px, AnyElement, AnyWindowHandle, App, AppContext, ClipboardItem, Context, Entity,
    Focusable, InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, SharedString,
    Styled, Subscription, Window, WindowBounds, WindowKind, WindowOptions,
};
use gpui_component::{
    badge::Badge,
    button::{Button, ButtonGroup},
    divider::Divider,
    h_flex,
    progress::Progress,
    v_flex, ActiveTheme, Root, Selectable, Sizable, ThemeMode, TitleBar,
};
use zenclash_core::{
    format_speed, LogMonitor, MihomoClient, MihomoProcess, TrafficMonitor, TrafficSnapshot,
};

mod actions;
mod bootstrap;
mod platform;
mod signal;
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
            NetworkTrayIcon, TrayClick, TrayCommand, TrayMenuState, TrayProxyGroup, TrayProxyNode,
        },
    },
    design::{apply_zen_theme, color, throughput_activity_percent, SIGNAL_CYAN, UPLINK_AMBER},
    pages::{
        proxies::ProxiesPage,
        runtime::{ProfileActivated, RuntimePage, RuntimePageServices},
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
            NavigateSubStore,
            NavigateNetwork,
            NavigateTraffic,
            NavigateSettings,
            SetRuleMode,
            SetGlobalMode,
            SetDirectMode,
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
    client: MihomoClient,
    traffic_monitor: Arc<TrafficMonitor>,
    traffic: TrafficSnapshot,
    upload_samples: VecDeque<u64>,
    download_samples: VecDeque<u64>,
    runtime: tokio::runtime::Handle,
    focus_handle: gpui::FocusHandle,
    proxies_page: Entity<ProxiesPage>,
    runtime_page: Entity<RuntimePage>,
    _mihomo_process: Option<Arc<MihomoProcess>>,
    profile_path: Option<PathBuf>,
    main_window: AnyWindowHandle,
    floating_window: Option<AnyWindowHandle>,
    tray_refreshing: bool,
    tray_refresh_pending: bool,
    tray_menu_requested: bool,
    system_proxy_commands: LatestCommandQueue<(bool, u16)>,
    tun_commands: LatestCommandQueue<bool>,
    proxy_selection_commands: LatestCommandQueue<(String, String)>,
    network_tray: Option<NetworkTrayIcon>,
    _subscriptions: Vec<Subscription>,
}

/// Runtime services prepared by the executable before constructing the UI.
pub struct AppServices {
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
    /// Tokio runtime used for network and blocking bridge tasks.
    pub runtime: tokio::runtime::Handle,
}

impl ZenClashApp {
    fn new(
        services: AppServices,
        network_tray: Option<NetworkTrayIcon>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let AppServices {
            client,
            traffic_monitor,
            log_monitor,
            mihomo_process,
            profile_path,
            runtime,
        } = services;
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let main_window = window.window_handle();
        let proxies_page = cx.new(|cx| ProxiesPage::new(client.clone(), runtime.clone(), cx));
        let app_profile_path = profile_path.clone();
        let runtime_page = cx.new(|cx| {
            RuntimePage::new(
                Page::Mihomo,
                RuntimePageServices {
                    client: client.clone(),
                    runtime: runtime.clone(),
                    traffic_monitor: traffic_monitor.clone(),
                    log_monitor,
                    process: mihomo_process.clone(),
                    profile_path,
                },
                window,
                cx,
            )
        });
        let profile_subscription =
            cx.subscribe(&runtime_page, |this, _, event: &ProfileActivated, cx| {
                this.profile_path = Some(event.path.clone());
                this.proxies_page
                    .update(cx, super::pages::proxies::ProxiesPage::profile_activated);
                this.refresh_tray_menu(cx);
            });
        let mut app = Self {
            current_page: Page::default(),
            outbound_mode: OutboundModeCoordinator::new_unsynchronized(OutboundMode::default()),
            client,
            traffic_monitor,
            traffic: TrafficSnapshot::default(),
            upload_samples: VecDeque::from(vec![0; 24]),
            download_samples: VecDeque::from(vec![0; 24]),
            runtime,
            focus_handle,
            proxies_page,
            runtime_page,
            _mihomo_process: mihomo_process,
            profile_path: app_profile_path,
            main_window,
            floating_window: None,
            tray_refreshing: false,
            tray_refresh_pending: false,
            tray_menu_requested: false,
            system_proxy_commands: LatestCommandQueue::default(),
            tun_commands: LatestCommandQueue::default(),
            proxy_selection_commands: LatestCommandQueue::default(),
            network_tray,
            _subscriptions: vec![profile_subscription],
        };
        app.start_traffic_updates(cx);
        app.start_mode_sync(cx);
        Self::start_tray_updates(cx);
        app.refresh_tray_menu(cx);
        app
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
                        this.refresh_tray_menu(cx);
                    }
                    if let Some(tray) = this.network_tray.as_mut() {
                        if let Err(error) = tray.update(&snapshot) {
                            tracing::warn!(%error, "failed to update native traffic tray");
                        }
                    }
                    if this.upload_samples.len() >= 24 {
                        this.upload_samples.pop_front();
                    }
                    if this.download_samples.len() >= 24 {
                        this.download_samples.pop_front();
                    }
                    this.upload_samples.push_back(snapshot.upload);
                    this.download_samples.push_back(snapshot.download);
                    this.traffic = snapshot;
                    cx.notify();
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
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let generation = mode.generation();
            let client = client.clone();
            let task = runtime.spawn(async move { client.runtime_config().await });
            match task.await {
                Ok(Ok(config)) => {
                    mode.synchronize(OutboundMode::from_api(&config.mode), generation);
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
