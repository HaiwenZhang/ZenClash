use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    actions, px, App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, WindowBounds, WindowOptions,
};
use gpui_component::{h_flex, ActiveTheme, Root, Theme, ThemeMode, TitleBar};
use zenclash_core::{LogMonitor, MihomoClient, MihomoProcess, TrafficMonitor, TrafficSnapshot};

use crate::{
    components::{
        sidebar::{OutboundMode, Sidebar},
        tray::NetworkTrayIcon,
    },
    pages::{proxies::ProxiesPage, runtime::RuntimePage, Page},
};

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
    ]
);

pub struct ZenClashApp {
    current_page: Page,
    outbound_mode: OutboundMode,
    client: MihomoClient,
    traffic_monitor: Arc<TrafficMonitor>,
    traffic: TrafficSnapshot,
    traffic_samples: VecDeque<u64>,
    runtime: tokio::runtime::Handle,
    focus_handle: gpui::FocusHandle,
    proxies_page: Entity<ProxiesPage>,
    runtime_page: Entity<RuntimePage>,
    _mihomo_process: Option<Arc<MihomoProcess>>,
    network_tray: Option<NetworkTrayIcon>,
}

pub struct AppServices {
    pub client: MihomoClient,
    pub traffic_monitor: Arc<TrafficMonitor>,
    pub log_monitor: Arc<LogMonitor>,
    pub mihomo_process: Option<Arc<MihomoProcess>>,
    pub profile_path: Option<PathBuf>,
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
        let proxies_page = cx.new(|cx| ProxiesPage::new(client.clone(), runtime.clone(), cx));
        let runtime_page = cx.new(|cx| {
            RuntimePage::new(
                Page::Mihomo,
                client.clone(),
                runtime.clone(),
                traffic_monitor.clone(),
                log_monitor,
                mihomo_process.clone(),
                profile_path,
                cx,
            )
        });
        let mut app = Self {
            current_page: Page::default(),
            outbound_mode: OutboundMode::default(),
            client,
            traffic_monitor,
            traffic: TrafficSnapshot::default(),
            traffic_samples: VecDeque::from(vec![0; 18]),
            runtime,
            focus_handle,
            proxies_page,
            runtime_page,
            _mihomo_process: mihomo_process,
            network_tray,
        };
        app.start_traffic_updates(cx);
        app
    }

    fn start_traffic_updates(&mut self, cx: &mut Context<Self>) {
        let monitor = self.traffic_monitor.clone();
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let snapshot = monitor.snapshot();
            if this
                .update(cx, |this, cx| {
                    let total = snapshot.upload.saturating_add(snapshot.download);
                    if let Some(tray) = this.network_tray.as_mut() {
                        tray.update(&snapshot);
                    }
                    this.traffic = snapshot;
                    if this.traffic_samples.len() >= 18 {
                        this.traffic_samples.pop_front();
                    }
                    this.traffic_samples.push_back(total);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        self.current_page = page;
        if page != Page::Proxies {
            self.runtime_page
                .update(cx, |runtime_page, cx| runtime_page.switch_to(page, cx));
        }
        cx.notify();
    }

    fn set_mode(&mut self, mode: OutboundMode, cx: &mut Context<Self>) {
        self.outbound_mode = mode;
        let api_mode = match mode {
            OutboundMode::Rule => "rule",
            OutboundMode::Global => "global",
            OutboundMode::Direct => "direct",
        };
        let client = self.client.clone();
        self.runtime.spawn(async move {
            if let Err(error) = client.set_mode(api_mode).await {
                tracing::warn!(%error, mode = api_mode, "failed to update Mihomo outbound mode");
            }
        });
        cx.notify();
    }

    fn on_navigate_system_proxy(
        &mut self,
        _: &NavigateSystemProxy,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::SystemProxy, cx);
    }

    fn on_navigate_tun(&mut self, _: &NavigateTun, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Tun, cx);
    }

    fn on_navigate_profiles(
        &mut self,
        _: &NavigateProfiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Profiles, cx);
    }

    fn on_navigate_proxies(&mut self, _: &NavigateProxies, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Proxies, cx);
    }

    fn on_navigate_mihomo(&mut self, _: &NavigateMihomo, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Mihomo, cx);
    }

    fn on_navigate_connections(
        &mut self,
        _: &NavigateConnections,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Connections, cx);
    }

    fn on_navigate_dns(&mut self, _: &NavigateDns, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Dns, cx);
    }

    fn on_navigate_sniffer(&mut self, _: &NavigateSniffer, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Sniffer, cx);
    }

    fn on_navigate_logs(&mut self, _: &NavigateLogs, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Logs, cx);
    }

    fn on_navigate_rules(&mut self, _: &NavigateRules, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Rules, cx);
    }

    fn on_navigate_resources(
        &mut self,
        _: &NavigateResources,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Resources, cx);
    }

    fn on_navigate_override(
        &mut self,
        _: &NavigateOverride,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Override, cx);
    }

    fn on_navigate_substore(
        &mut self,
        _: &NavigateSubStore,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::SubStore, cx);
    }

    fn on_navigate_network(&mut self, _: &NavigateNetwork, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Network, cx);
    }

    fn on_navigate_traffic(&mut self, _: &NavigateTraffic, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Traffic, cx);
    }

    fn on_navigate_settings(
        &mut self,
        _: &NavigateSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Settings, cx);
    }

    fn on_set_rule_mode(&mut self, _: &SetRuleMode, _: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(OutboundMode::Rule, cx);
    }

    fn on_set_global_mode(&mut self, _: &SetGlobalMode, _: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(OutboundMode::Global, cx);
    }

    fn on_set_direct_mode(&mut self, _: &SetDirectMode, _: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(OutboundMode::Direct, cx);
    }

    fn on_set_light_theme(
        &mut self,
        _: &SetLightTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Theme::change(ThemeMode::Light, Some(window), cx);
        cx.notify();
    }

    fn on_set_dark_theme(&mut self, _: &SetDarkTheme, window: &mut Window, cx: &mut Context<Self>) {
        Theme::change(ThemeMode::Dark, Some(window), cx);
        cx.notify();
    }

    fn on_show_traffic_icon(
        &mut self,
        _: &ShowTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray {
            tray.set_visible(true);
        }
        cx.notify();
    }

    fn on_hide_traffic_icon(
        &mut self,
        _: &HideTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray {
            tray.set_visible(false);
        }
        cx.notify();
    }
}

impl Focusable for ZenClashApp {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZenClashApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let samples = self.traffic_samples.iter().copied().collect::<Vec<_>>();
        let content = match self.current_page {
            Page::Proxies => self.proxies_page.clone().into_any_element(),
            _ => self.runtime_page.clone().into_any_element(),
        };

        h_flex()
            .id("zenclash-app")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("ZenClash")
            .on_action(cx.listener(Self::on_navigate_system_proxy))
            .on_action(cx.listener(Self::on_navigate_tun))
            .on_action(cx.listener(Self::on_navigate_profiles))
            .on_action(cx.listener(Self::on_navigate_proxies))
            .on_action(cx.listener(Self::on_navigate_mihomo))
            .on_action(cx.listener(Self::on_navigate_connections))
            .on_action(cx.listener(Self::on_navigate_dns))
            .on_action(cx.listener(Self::on_navigate_sniffer))
            .on_action(cx.listener(Self::on_navigate_logs))
            .on_action(cx.listener(Self::on_navigate_rules))
            .on_action(cx.listener(Self::on_navigate_resources))
            .on_action(cx.listener(Self::on_navigate_override))
            .on_action(cx.listener(Self::on_navigate_substore))
            .on_action(cx.listener(Self::on_navigate_network))
            .on_action(cx.listener(Self::on_navigate_traffic))
            .on_action(cx.listener(Self::on_navigate_settings))
            .on_action(cx.listener(Self::on_set_rule_mode))
            .on_action(cx.listener(Self::on_set_global_mode))
            .on_action(cx.listener(Self::on_set_direct_mode))
            .on_action(cx.listener(Self::on_set_light_theme))
            .on_action(cx.listener(Self::on_set_dark_theme))
            .on_action(cx.listener(Self::on_show_traffic_icon))
            .on_action(cx.listener(Self::on_hide_traffic_icon))
            .child(Sidebar::new(
                self.current_page,
                self.outbound_mode,
                self.traffic.clone(),
                samples.clone(),
            ))
            .child(gpui::div().flex_1().h_full().min_w_0().child(content))
    }
}

pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

pub fn create_main_window(services: AppServices, cx: &mut App) -> anyhow::Result<()> {
    let title = SharedString::from("ZenClash");
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(gpui::size(px(1180.), px(780.)), cx)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    cx.spawn(async move |cx| {
        cx.open_window(options, |window, cx| {
            Theme::change(ThemeMode::Dark, Some(window), cx);
            window.set_window_title(&title);
            window.activate_window();
            let network_tray = match NetworkTrayIcon::new() {
                Ok(tray) => Some(tray),
                Err(error) => {
                    tracing::warn!(%error, "failed to create native traffic tray icon");
                    None
                }
            };
            let app = cx.new(|cx| ZenClashApp::new(services, network_tray, window, cx));
            cx.new(|cx| Root::new(app, window, cx))
        })
        .expect("failed to open ZenClash window");
    })
    .detach();

    Ok(())
}
