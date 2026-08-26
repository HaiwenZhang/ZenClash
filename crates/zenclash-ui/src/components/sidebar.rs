use gpui::{
    div, prelude::FluentBuilder, px, App, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    badge::Badge, divider::Divider, scroll::ScrollableElement, v_flex, ActiveTheme, Icon, IconName,
};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateLogs, NavigateMihomo, NavigateNetwork,
        NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources, NavigateRules,
        NavigateSettings, NavigateSniffer, NavigateSubStore, NavigateSystemProxy, NavigateTraffic,
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

    fn render_nav_item(&self, page: Page, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let active = page == self.current_page;
        div()
            .id(page.route())
            .relative()
            .h(px(40.))
            .w_full()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_color(if active {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .when(active, |this| this.bg(theme.sidebar_accent))
            .when(active && theme.shadow, |this| this.shadow_xs())
            .when(!active, |this| {
                this.hover(|style| {
                    style
                        .bg(theme.sidebar_accent.opacity(0.62))
                        .text_color(theme.foreground)
                })
            })
            .when(active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(8.))
                        .bottom(px(8.))
                        .w(px(3.))
                        .rounded_r_full()
                        .bg(theme.primary),
                )
            })
            .child(
                gpui_component::h_flex()
                    .size_full()
                    .px_3()
                    .gap_3()
                    .child(
                        Badge::new()
                            .when(active, |badge| badge.dot().color(theme.primary))
                            .child(
                                div()
                                    .size(px(27.))
                                    .rounded(px(6.))
                                    .bg(if active {
                                        theme.primary.opacity(0.13)
                                    } else {
                                        theme.transparent
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(Icon::new(page.icon()).size_4()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(if active {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .child(page.label()),
                    )
                    .when(active, |this| {
                        this.child(
                            Icon::new(IconName::ChevronRight)
                                .size_3()
                                .text_color(theme.primary),
                        )
                    }),
            )
            .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
            .into_any_element()
    }

    fn render_section(
        &self,
        label: &'static str,
        pages: &[Page],
        theme: &gpui_component::Theme,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .pb_1()
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground.opacity(0.72))
                    .child(label),
            )
            .children(
                pages
                    .iter()
                    .copied()
                    .map(|page| self.render_nav_item(page, theme)),
            )
            .into_any_element()
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .w(px(216.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px_2()
                    .py_3()
                    .child(
                        v_flex()
                            .gap_3()
                            .child(self.render_section("OVERVIEW", &Page::OVERVIEW, theme))
                            .child(Divider::horizontal().color(theme.sidebar_border))
                            .child(self.render_section("ROUTING", &Page::ROUTING, theme))
                            .child(Divider::horizontal().color(theme.sidebar_border))
                            .child(self.render_section(
                                "CONFIGURATION",
                                &Page::CONFIGURATION,
                                theme,
                            ))
                            .child(Divider::horizontal().color(theme.sidebar_border))
                            .child(self.render_section("SYSTEM", &Page::SYSTEM, theme)),
                    ),
            )
            .child(
                div()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(self.render_nav_item(Page::Settings, theme)),
            )
    }
}

fn dispatch_navigate(page: Page, window: &mut Window, cx: &mut App) {
    match page {
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
        Page::SubStore => window.dispatch_action(Box::new(NavigateSubStore), cx),
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
