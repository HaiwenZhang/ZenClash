use gpui::{
    div, prelude::FluentBuilder as _, rems, App, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, StatefulInteractiveElement as _, Styled, Window,
};
use gpui_component::{
    h_flex, sidebar::Sidebar as GpuiSidebar, v_flex, ActiveTheme, Collapsible, Icon,
};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateHome, NavigateLogs, NavigateMihomo,
        NavigateNetwork, NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources,
        NavigateRules, NavigateSettings, NavigateSniffer, NavigateSystemProxy, NavigateTraffic,
        NavigateTun,
    },
    assets::ZENCLASH_MARK_PATH,
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
}

impl Sidebar {
    /// Creates a sidebar with the supplied destination highlighted.
    #[must_use]
    pub const fn new(current_page: Page) -> Self {
        Self { current_page }
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
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let accent = theme.sidebar_accent;
        let accent_foreground = theme.sidebar_accent_foreground;
        let radius = theme.radius_lg;

        v_flex()
            .gap_2()
            .children(self.pages.into_iter().map(|page| {
                let active = page == self.current_page;

                h_flex()
                    .id(page.route())
                    .w_full()
                    .h(rems(3.))
                    .px_3()
                    .gap_3()
                    .overflow_x_hidden()
                    .cursor_pointer()
                    .rounded(radius)
                    .text_base()
                    .when(active, |this| {
                        this.bg(accent)
                            .text_color(accent_foreground)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .when(!active, |this| {
                        this.hover(move |this| {
                            this.bg(accent.opacity(0.82)).text_color(accent_foreground)
                        })
                    })
                    .child(Icon::new(page.icon()).size(rems(1.25)))
                    .when(!self.collapsed, |this| {
                        this.child(div().flex_1().overflow_x_hidden().child(page.label()))
                    })
                    .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
            }))
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        let navigation = std::iter::once(Page::Home).chain(Page::PRIMARY);

        GpuiSidebar::left()
            .w(rems(15.))
            .collapsible(false)
            .header(
                h_flex()
                    .h(rems(5.25))
                    .w_full()
                    .pt_8()
                    .gap_3()
                    .child(
                        div()
                            .size(rems(3.))
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
            .child(SidebarNavigation::new(self.current_page, navigation))
            .footer(
                div()
                    .w_full()
                    .child(SidebarNavigation::new(self.current_page, [Page::Settings])),
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
