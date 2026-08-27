use gpui::{
    App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, prelude::FluentBuilder as _,
    rems,
};
use gpui_component::{
    ActiveTheme, Collapsible, Icon, IconName, Selectable, Sizable, button::Button,
    button::ButtonVariants, h_flex, sidebar::Sidebar as GpuiSidebar, v_flex,
};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateHome, NavigateLogs, NavigateMihomo,
        NavigateNetwork, NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources,
        NavigateRules, NavigateSettings, NavigateSniffer, NavigateSystemProxy, NavigateTraffic,
        NavigateTun, ToggleSidebar,
    },
    assets::{AppIcon, GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH, ZENCLASH_MARK_PATH},
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
    pub fn label(self) -> String {
        zenclash_i18n::text(match self {
            Self::Rule => "outbound_mode.rule",
            Self::Global => "outbound_mode.global",
            Self::Direct => "outbound_mode.direct",
        })
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
    collapsed: bool,
}

impl Sidebar {
    /// Creates a sidebar with the supplied destination highlighted.
    #[must_use]
    pub const fn new(current_page: Page) -> Self {
        Self {
            current_page,
            collapsed: false,
        }
    }

    /// Sets whether only navigation icons are visible.
    #[must_use]
    pub const fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

#[derive(IntoElement)]
struct SidebarNavigation {
    current_page: Page,
    pages: Vec<Page>,
    collapsed: bool,
}

impl SidebarNavigation {
    fn new(current_page: Page, pages: impl IntoIterator<Item = Page>) -> Self {
        Self {
            current_page,
            pages: pages.into_iter().collect(),
            collapsed: false,
        }
    }
}

impl Collapsible for SidebarNavigation {
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
}

impl RenderOnce for SidebarNavigation {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        v_flex()
            .gap_2()
            .children(self.pages.into_iter().map(|page| {
                let active = page == self.current_page;

                Button::new(page.route())
                    .icon(sidebar_icon(page).size(rems(1.25)))
                    .w_full()
                    .h(rems(3.))
                    .justify_start()
                    .when(self.collapsed, |this| this.justify_center())
                    .ghost()
                    .selected(active)
                    .when(!self.collapsed, |this| this.label(page.label()))
                    .tooltip(page.label())
                    .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
            }))
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let navigation = std::iter::once(Page::Home).chain(Page::PRIMARY);
        let toggle_icon = if self.collapsed {
            IconName::PanelLeftOpen
        } else {
            IconName::PanelLeftClose
        };
        let toggle_label = zenclash_i18n::text(if self.collapsed {
            "sidebar.expand"
        } else {
            "sidebar.collapse"
        });

        GpuiSidebar::left()
            .w(rems(15.))
            .collapsible(true)
            .collapsed(self.collapsed)
            .header(
                h_flex()
                    .h(rems(5.25))
                    .w_full()
                    .pt_8()
                    .when(!self.collapsed, |this| {
                        this.gap_3().justify_between().child(
                            h_flex()
                                .min_w_0()
                                .gap_3()
                                .child(
                                    div()
                                        .size(rems(3.))
                                        .flex_shrink_0()
                                        .rounded(theme.radius_lg)
                                        .bg(theme.sidebar_foreground.opacity(0.065))
                                        .border_1()
                                        .border_color(theme.sidebar_foreground.opacity(0.08))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::empty()
                                                .path(ZENCLASH_MARK_PATH)
                                                .size(rems(1.8))
                                                .text_color(theme.muted_foreground),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(zenclash_i18n::text("app.name")),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .child(zenclash_i18n::text("app.description")),
                                        ),
                                ),
                        )
                    })
                    .when(self.collapsed, |this| this.justify_center())
                    .child(
                        Button::new("toggle-sidebar")
                            .icon(toggle_icon)
                            .small()
                            .ghost()
                            .tooltip(toggle_label)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleSidebar), cx);
                            }),
                    ),
            )
            .child(SidebarNavigation::new(self.current_page, navigation))
            .footer(
                div()
                    .w_full()
                    .child(SidebarNavigation::new(self.current_page, [Page::Settings])),
            )
    }
}

fn sidebar_icon(page: Page) -> Icon {
    if page == Page::Home {
        Icon::new(AppIcon::House)
    } else if let Some(path) = sidebar_icon_path(page) {
        Icon::empty().path(path)
    } else {
        Icon::new(page.icon())
    }
}

const fn sidebar_icon_path(page: Page) -> Option<&'static str> {
    match page {
        Page::Proxies => Some(GROUP_ICON_PATH),
        Page::Connections => Some(RADIO_ICON_PATH),
        Page::Rules => Some(RULER_ICON_PATH),
        _ => None,
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
    use crate::{
        assets::{GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH},
        pages::Page,
    };

    use super::{OutboundMode, Sidebar, sidebar_icon_path};

    #[test]
    fn sidebar_defaults_to_expanded_and_accepts_collapsed_state() {
        assert!(!Sidebar::new(Page::Home).collapsed);
        assert!(Sidebar::new(Page::Home).collapsed(true).collapsed);
    }

    #[test]
    fn sidebar_uses_requested_custom_icons() {
        assert_eq!(sidebar_icon_path(Page::Proxies), Some(GROUP_ICON_PATH));
        assert_eq!(sidebar_icon_path(Page::Connections), Some(RADIO_ICON_PATH));
        assert_eq!(sidebar_icon_path(Page::Rules), Some(RULER_ICON_PATH));
        assert_eq!(sidebar_icon_path(Page::Home), None);
    }

    #[test]
    fn outbound_mode_parses_mihomo_values_case_insensitively() {
        assert_eq!(OutboundMode::from_api("GLOBAL"), OutboundMode::Global);
    }

    #[test]
    fn outbound_mode_defaults_unknown_values_to_rule() {
        assert_eq!(OutboundMode::from_api("unexpected"), OutboundMode::Rule);
    }
}
