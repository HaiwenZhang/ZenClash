use gpui::{div, rems, App, IntoElement, ParentElement, RenderOnce, Styled, Window};
use gpui_component::{
    h_flex,
    sidebar::{Sidebar as GpuiSidebar, SidebarMenu, SidebarMenuItem},
    v_flex, ActiveTheme, Icon, IconName,
};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateHome, NavigateLogs, NavigateMihomo,
        NavigateNetwork, NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources,
        NavigateRules, NavigateSettings, NavigateSniffer, NavigateSystemProxy, NavigateTraffic,
        NavigateTun,
    },
    pages::Page,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Mihomo's mutually exclusive outbound routing modes.
pub enum OutboundMode {
    /// Route connections through Mihomo rules.
    #[default]
    Rule,
    /// Route connections through the selected global proxy.
    Global,
    /// Bypass proxies for every connection.
    Direct,
}

impl OutboundMode {
    /// Returns the localized user-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rule => "规则",
            Self::Global => "全局",
            Self::Direct => "直连",
        }
    }

    /// Returns the compact uppercase UI code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rule => "RULE",
            Self::Global => "GLOBAL",
            Self::Direct => "DIRECT",
        }
    }

    /// Returns the lowercase value accepted by Mihomo's `/configs` API.
    #[must_use]
    pub const fn api_value(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    /// Parses a Mihomo API value, defaulting unknown values to rule mode.
    #[must_use]
    pub fn from_api(mode: &str) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "global" => Self::Global,
            "direct" => Self::Direct,
            _ => Self::Rule,
        }
    }
}

#[derive(IntoElement)]
/// Primary page navigation rendered beside the active content view.
pub struct Sidebar {
    current_page: Page,
}

impl Sidebar {
    /// Creates a sidebar with the supplied destination highlighted.
    #[must_use]
    pub const fn new(current_page: Page) -> Self {
        Self { current_page }
    }

    fn menu_item(&self, page: Page) -> SidebarMenuItem {
        SidebarMenuItem::new(page.label())
            .icon(page.icon())
            .active(page == self.current_page)
            .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        let navigation = std::iter::once(Page::Home).chain(Page::PRIMARY);

        GpuiSidebar::left()
            .w(rems(14.))
            .collapsible(false)
            .header(
                h_flex()
                    .h(rems(4.5))
                    .w_full()
                    .pt_8()
                    .gap_3()
                    .child(
                        div()
                            .size_8()
                            .rounded(theme.radius)
                            .bg(theme.primary.opacity(0.13))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Globe)
                                    .size_4()
                                    .text_color(theme.primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("ZenClash"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Mihomo 桌面客户端"),
                            ),
                    ),
            )
            .child(SidebarMenu::new().children(navigation.map(|page| self.menu_item(page))))
            .footer(
                div()
                    .w_full()
                    .child(SidebarMenu::new().child(self.menu_item(Page::Settings))),
            )
    }
}

pub(crate) fn dispatch_navigate(page: Page, window: &mut Window, cx: &mut App) {
    match page {
        Page::Home => window.dispatch_action(Box::new(NavigateHome), cx),
        Page::SystemProxy => window.dispatch_action(Box::new(NavigateSystemProxy), cx),
        Page::Tun => window.dispatch_action(Box::new(NavigateTun), cx),
        Page::Profiles => window.dispatch_action(Box::new(NavigateProfiles), cx),
        Page::Proxies => window.dispatch_action(Box::new(NavigateProxies), cx),
        Page::Mihomo => window.dispatch_action(Box::new(NavigateMihomo), cx),
        Page::Connections => window.dispatch_action(Box::new(NavigateConnections), cx),
        Page::Dns => window.dispatch_action(Box::new(NavigateDns), cx),
        Page::Sniffer => window.dispatch_action(Box::new(NavigateSniffer), cx),
        Page::Logs => window.dispatch_action(Box::new(NavigateLogs), cx),
        Page::Rules => window.dispatch_action(Box::new(NavigateRules), cx),
        Page::Resources => window.dispatch_action(Box::new(NavigateResources), cx),
        Page::Override => window.dispatch_action(Box::new(NavigateOverride), cx),
        Page::Network => window.dispatch_action(Box::new(NavigateNetwork), cx),
        Page::Traffic => window.dispatch_action(Box::new(NavigateTraffic), cx),
        Page::Settings => window.dispatch_action(Box::new(NavigateSettings), cx),
    }
}

#[cfg(test)]
mod tests {
    use super::OutboundMode;

    #[test]
    fn outbound_mode_parses_mihomo_values_case_insensitively() {
        assert_eq!(OutboundMode::from_api("GLOBAL"), OutboundMode::Global);
    }

    #[test]
    fn outbound_mode_defaults_unknown_values_to_rule() {
        assert_eq!(OutboundMode::from_api("unexpected"), OutboundMode::Rule);
    }
}
