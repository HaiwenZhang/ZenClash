use super::{
    apply_zen_theme, AppPreferences, AppearancePreference, Context, HideTrafficIcon,
    NavigateConnections, NavigateDns, NavigateLogs, NavigateMihomo, NavigateNetwork,
    NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources, NavigateRules,
    NavigateSettings, NavigateSniffer, NavigateSubStore, NavigateSystemProxy, NavigateTraffic,
    NavigateTun, OutboundMode, Page, Quit, SetDarkTheme, SetDirectMode, SetGlobalMode,
    SetLightTheme, SetRuleMode, SetSystemTheme, ShowStatusMenu, ShowTrafficIcon, ThemeMode,
    ToggleFloatingWindow, Window, ZenClashApp,
};

impl ZenClashApp {
    pub(super) fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.begin_quit(None, cx);
    }

    fn update_preferences(&mut self, mutate: impl Fn(&mut AppPreferences)) {
        mutate(&mut self.preferences);
        let Some(store) = &self.preferences_store else {
            return;
        };
        match store.update(mutate) {
            Ok(preferences) => self.preferences = preferences,
            Err(error) => {
                tracing::warn!(%error, path = %store.path().display(), "failed to update application preferences");
            }
        }
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

    pub(super) fn on_navigate_substore(
        &mut self,
        _: &NavigateSubStore,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::SubStore, cx);
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
        self.set_mode(OutboundMode::Rule, cx);
    }

    pub(super) fn on_set_global_mode(
        &mut self,
        _: &SetGlobalMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_mode(OutboundMode::Global, cx);
    }

    pub(super) fn on_set_direct_mode(
        &mut self,
        _: &SetDirectMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_mode(OutboundMode::Direct, cx);
    }

    pub(super) fn on_set_light_theme(
        &mut self,
        _: &SetLightTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        self.update_preferences(|preferences| {
            preferences.appearance = AppearancePreference::Light;
        });
        cx.notify();
    }

    pub(super) fn on_set_system_theme(
        &mut self,
        _: &SetSystemTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::from(window.appearance()), Some(window), cx);
        self.update_preferences(|preferences| {
            preferences.appearance = AppearancePreference::System;
        });
        cx.notify();
    }

    pub(super) fn on_set_dark_theme(
        &mut self,
        _: &SetDarkTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        apply_zen_theme(ThemeMode::Dark, Some(window), cx);
        self.update_preferences(|preferences| {
            preferences.appearance = AppearancePreference::Dark;
        });
        cx.notify();
    }

    pub(super) fn on_show_traffic_icon(
        &mut self,
        _: &ShowTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray {
            if let Err(error) = tray.set_visible(true) {
                tracing::warn!(%error, "failed to show native traffic tray");
            }
        }
        self.update_preferences(|preferences| preferences.traffic_tray_visible = true);
        cx.notify();
    }

    pub(super) fn on_hide_traffic_icon(
        &mut self,
        _: &HideTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.network_tray {
            if let Err(error) = tray.set_visible(false) {
                tracing::warn!(%error, "failed to hide native traffic tray");
            }
        }
        self.update_preferences(|preferences| preferences.traffic_tray_visible = false);
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

    pub(super) fn on_toggle_floating_window(
        &mut self,
        _: &ToggleFloatingWindow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_floating_window(cx);
    }
}
