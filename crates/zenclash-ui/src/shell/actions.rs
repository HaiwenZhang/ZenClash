use crate::foundation::actions::HideTrafficIcon;
use crate::foundation::actions::NavigateConnections;
use crate::foundation::actions::NavigateDns;
use crate::foundation::actions::NavigateHome;
use crate::foundation::actions::NavigateLogs;
use crate::foundation::actions::NavigateMihomo;
use crate::foundation::actions::NavigateNetwork;
use crate::foundation::actions::NavigateOverride;
use crate::foundation::actions::NavigateProfiles;
use crate::foundation::actions::NavigateProxies;
use crate::foundation::actions::NavigateResources;
use crate::foundation::actions::NavigateRules;
use crate::foundation::actions::NavigateSettings;
use crate::foundation::actions::NavigateSniffer;
use crate::foundation::actions::NavigateSystemProxy;
use crate::foundation::actions::NavigateTraffic;
use crate::foundation::actions::NavigateTun;
use crate::foundation::actions::Page;
use crate::foundation::actions::Quit;
use crate::foundation::actions::SetDarkTheme;
use crate::foundation::actions::SetDirectMode;
use crate::foundation::actions::SetGlobalMode;
use crate::foundation::actions::SetLightTheme;
use crate::foundation::actions::SetRuleMode;
use crate::foundation::actions::SetSystemTheme;
use crate::foundation::actions::ShowStatusMenu;
use crate::foundation::actions::ShowTrafficIcon;
use crate::foundation::actions::ToggleSidebar;
use crate::foundation::design::apply_zen_theme;
use crate::outbound_mode::OutboundMode;
use crate::shell::app::ZenClashApp;
use crate::shell::controllers::menu_preferences::MenuPreferenceChange;
use gpui_kit::Context;
use gpui_kit::Window;
use gpui_kit::component::ThemeMode;
use zenclash_core::AppearancePreference;

impl ZenClashApp {
    pub(in crate::shell) fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.begin_quit(None, cx);
    }

    pub(in crate::shell) fn on_navigate_home(
        &mut self,
        _: &NavigateHome,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Home, cx);
    }

    pub(in crate::shell) fn on_navigate_system_proxy(
        &mut self,
        _: &NavigateSystemProxy,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::SystemProxy, cx);
    }

    pub(in crate::shell) fn on_navigate_tun(
        &mut self,
        _: &NavigateTun,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Tun, cx);
    }

    pub(in crate::shell) fn on_navigate_profiles(
        &mut self,
        _: &NavigateProfiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Profiles, cx);
    }

    pub(in crate::shell) fn on_navigate_proxies(
        &mut self,
        _: &NavigateProxies,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Proxies, cx);
    }

    pub(in crate::shell) fn on_navigate_mihomo(
        &mut self,
        _: &NavigateMihomo,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Mihomo, cx);
    }

    pub(in crate::shell) fn on_navigate_connections(
        &mut self,
        _: &NavigateConnections,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Connections, cx);
    }

    pub(in crate::shell) fn on_navigate_dns(
        &mut self,
        _: &NavigateDns,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Dns, cx);
    }

    pub(in crate::shell) fn on_navigate_sniffer(
        &mut self,
        _: &NavigateSniffer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Sniffer, cx);
    }

    pub(in crate::shell) fn on_navigate_logs(
        &mut self,
        _: &NavigateLogs,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Logs, cx);
    }

    pub(in crate::shell) fn on_navigate_rules(
        &mut self,
        _: &NavigateRules,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Rules, cx);
    }

    pub(in crate::shell) fn on_navigate_resources(
        &mut self,
        _: &NavigateResources,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Resources, cx);
    }

    pub(in crate::shell) fn on_navigate_override(
        &mut self,
        _: &NavigateOverride,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Override, cx);
    }

    pub(in crate::shell) fn on_navigate_network(
        &mut self,
        _: &NavigateNetwork,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Network, cx);
    }

    pub(in crate::shell) fn on_navigate_traffic(
        &mut self,
        _: &NavigateTraffic,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Traffic, cx);
    }

    pub(in crate::shell) fn on_navigate_settings(
        &mut self,
        _: &NavigateSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Page::Settings, cx);
    }

    pub(in crate::shell) fn on_set_rule_mode(
        &mut self,
        _: &SetRuleMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Rule, cx);
    }

    pub(in crate::shell) fn on_set_global_mode(
        &mut self,
        _: &SetGlobalMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Global, cx);
    }

    pub(in crate::shell) fn on_set_direct_mode(
        &mut self,
        _: &SetDirectMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_outbound_mode(OutboundMode::Direct, cx);
    }

    pub(in crate::shell) fn on_set_light_theme(
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

    pub(in crate::shell) fn on_set_system_theme(
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

    pub(in crate::shell) fn on_set_dark_theme(
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

    pub(in crate::shell) fn on_show_traffic_icon(
        &mut self,
        _: &ShowTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.tray.icon
            && let Err(error) = tray.set_visible(true)
        {
            tracing::warn!(%error, "failed to show native traffic tray");
        }
        self.update_menu_preference(MenuPreferenceChange::TrayVisibility(true), cx);
        cx.notify();
    }

    pub(in crate::shell) fn on_hide_traffic_icon(
        &mut self,
        _: &HideTrafficIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tray) = &self.tray.icon
            && let Err(error) = tray.set_visible(false)
        {
            tracing::warn!(%error, "failed to hide native traffic tray");
        }
        self.update_menu_preference(MenuPreferenceChange::TrayVisibility(false), cx);
        cx.notify();
    }

    pub(in crate::shell) fn on_show_status_menu(
        &mut self,
        _: &ShowStatusMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tray.menu_requested = true;
        self.refresh_tray_menu(cx);
    }

    pub(in crate::shell) fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }
}
