use super::menu_preferences::MenuPreferenceChange;
use super::{
    AppearancePreference, Context, HideTrafficIcon, NavigateConnections, NavigateDns, NavigateHome,
    NavigateLogs, NavigateMihomo, NavigateNetwork, NavigateOverride, NavigateProfiles,
    NavigateProxies, NavigateResources, NavigateRules, NavigateSettings, NavigateSniffer,
    NavigateSystemProxy, NavigateTraffic, NavigateTun, OutboundMode, Page, Quit, SetDarkTheme,
    SetDirectMode, SetGlobalMode, SetLightTheme, SetRuleMode, SetSystemTheme, ShowStatusMenu,
    ShowTrafficIcon, ThemeMode, ToggleSidebar, Window, ZenClashApp, apply_zen_theme,
};

impl ZenClashApp {
    pub(super) fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.begin_quit(None, cx);
    }

    pub(super) fn on_navigate_home(
        &mut self,
        _: &NavigateHome,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Home, cx);
    }

    pub(super) fn on_navigate_system_proxy(
        &mut self,
        _: &NavigateSystemProxy,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::SystemProxy, cx);
    }

    pub(super) fn on_navigate_tun(
        &mut self,
        _: &NavigateTun,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Tun, cx);
    }

    pub(super) fn on_navigate_profiles(
        &mut self,
        _: &NavigateProfiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Profiles, cx);
    }

    pub(super) fn on_navigate_proxies(
        &mut self,
        _: &NavigateProxies,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Proxies, cx);
    }

    pub(super) fn on_navigate_mihomo(
        &mut self,
        _: &NavigateMihomo,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Mihomo, cx);
    }

    pub(super) fn on_navigate_connections(
        &mut self,
        _: &NavigateConnections,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Connections, cx);
    }

    pub(super) fn on_navigate_dns(
        &mut self,
        _: &NavigateDns,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Dns, cx);
    }

    pub(super) fn on_navigate_sniffer(
        &mut self,
        _: &NavigateSniffer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Sniffer, cx);
    }

    pub(super) fn on_navigate_logs(
        &mut self,
        _: &NavigateLogs,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Logs, cx);
    }

    pub(super) fn on_navigate_rules(
        &mut self,
        _: &NavigateRules,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Rules, cx);
    }

    pub(super) fn on_navigate_resources(
        &mut self,
        _: &NavigateResources,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Resources, cx);
    }

    pub(super) fn on_navigate_override(
        &mut self,
        _: &NavigateOverride,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Override, cx);
    }

    pub(super) fn on_navigate_network(
        &mut self,
        _: &NavigateNetwork,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Network, cx);
    }

    pub(super) fn on_navigate_traffic(
        &mut self,
        _: &NavigateTraffic,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Traffic, cx);
    }

    pub(super) fn on_navigate_settings(
        &mut self,
        _: &NavigateSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Settings, cx);
    }

    pub(super) fn on_set_rule_mode(
        &mut self,
        _: &SetRuleMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Rule, cx);
    }

    pub(super) fn on_set_global_mode(
        &mut self,
        _: &SetGlobalMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Global, cx);
    }

    pub(super) fn on_set_direct_mode(
        &mut self,
        _: &SetDirectMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Direct, cx);
    }

    pub(super) fn on_set_light_theme(
        &mut self,
        _: &SetLightTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        self.update_menu_preference(
            MenuPreferenceChange::Appearance(AppearancePreference::Light),
            cx,
        );
        cx.notify();
    }

    pub(super) fn on_set_system_theme(
        &mut self,
        _: &SetSystemTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::from(window.appearance()), Some(window), cx);
        self.update_menu_preference(
            MenuPreferenceChange::Appearance(AppearancePreference::System),
            cx,
        );
        cx.notify();
    }

    pub(super) fn on_set_dark_theme(
        &mut self,
        _: &SetDarkTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::Dark, Some(window), cx);
        self.update_menu_preference(
            MenuPreferenceChange::Appearance(AppearancePreference::Dark),
            cx,
        );
        cx.notify();
    }

    pub(in crate::app) fn set_tray_display(
        &mut self,
        display: zenclash_core::TrayDisplayPreference,
        cx: &mut Context<Self>,
    ) {
        let traffic = self.traffic_monitor.snapshot();
        if let Some(tray) = self.network_tray.as_mut()
            && let Err(error) = tray.set_display(display, &traffic)
        {
            self.tray_command_error = Some(error);
            return;
        }
        self.update_menu_preference(MenuPreferenceChange::TrayDisplay(display), cx);
        self.tray_state.display = display;
        if let Some(tray) = self.network_tray.as_mut()
            && let Err(error) =
                tray.update_menu_with_observation(&self.tray_state, self.tray_observation.clone())
        {
            tracing::warn!(%error, "failed to refresh tray display choices");
        }
        cx.notify();
    }

    pub(super) fn on_show_traffic_icon(
        &mut self,
        _: &ShowTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray
            && let Err(error) = tray.set_visible(true)
        {
            tracing::warn!(%error, "failed to show native traffic tray");
        }
        self.update_menu_preference(MenuPreferenceChange::TrayVisibility(true), cx);
        cx.notify();
    }

    pub(super) fn on_hide_traffic_icon(
        &mut self,
        _: &HideTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray
            && let Err(error) = tray.set_visible(false)
        {
            tracing::warn!(%error, "failed to hide native traffic tray");
        }
        self.update_menu_preference(MenuPreferenceChange::TrayVisibility(false), cx);
        cx.notify();
    }

    pub(super) fn on_show_status_menu(
        &mut self,
        _: &ShowStatusMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tray_menu_requested = true;
        self.refresh_tray_menu(cx);
    }

    pub(super) fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }
}
