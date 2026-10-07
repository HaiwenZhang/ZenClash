use gpui_kit::component::{
    ActiveTheme, Collapsible, Icon, IconName, Selectable, Sizable,
    button::ButtonVariants,
    button::{Button, ButtonCustomVariant},
    h_flex,
    sidebar::{Sidebar as GpuiSidebar, SidebarItem},
    v_flex,
};
use gpui_kit::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Styled,
    TestSupportExt, Window, div, prelude::FluentBuilder as _, rems,
};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateHome, NavigateLogs, NavigateMihomo,
        NavigateNetwork, NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources,
        NavigateRules, NavigateSettings, NavigateSniffer, NavigateSystemProxy, NavigateTraffic,
        NavigateTun, SetDarkTheme, SetLightTheme, ToggleSidebar,
    },
    assets::{AppIcon, GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH},
    pages::Page,
};

pub(crate) fn runtime_uptime(process: Option<&zenclash_core::ProcessStatus>) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let seconds = process?.uptime_secs_at(now)?;
    Some(zenclash_i18n::text_with(
        "app.status.uptime",
        &[(
            "time",
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            ),
        )],
    ))
}

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
    status: Option<(String, String, bool)>,
}

impl Sidebar {
    /// Creates a sidebar with the supplied destination highlighted.
    #[must_use]
    pub const fn new(current_page: Page) -> Self {
        Self {
            current_page,
            collapsed: false,
            status: None,
        }
    }

    pub(crate) fn status(mut self, core: String, label: String, connected: bool) -> Self {
        self.status = Some((core, label, connected));
        self
    }

    /// Sets whether only navigation icons are visible.
    #[must_use]
    pub const fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

#[derive(Clone, IntoElement)]
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

impl SidebarItem for SidebarNavigation {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        div().id(id).child(RenderOnce::render(self, window, cx))
    }
}

impl RenderOnce for SidebarNavigation {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .gap_1()
            .children(self.pages.into_iter().map(|page| {
                let active = page == self.current_page.navigation_parent();

                Button::new(page.route())
                    .accessibility_label(page.label())
                    .w_full()
                    .h(rems(2.75))
                    .justify_start()
                    .when(self.collapsed, |this| this.justify_center().px_1())
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .foreground(if active {
                                cx.theme().primary
                            } else {
                                cx.theme().sidebar_foreground
                            })
                            .hover(cx.theme().sidebar_accent)
                            .active(cx.theme().sidebar_accent),
                    )
                    .selected(active)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .when(self.collapsed, |this| this.justify_center())
                            .child(
                                div()
                                    .id(format!("sidebar-icon:{}", page.route()))
                                    .test_support()
                                    .size(rems(1.25))
                                    .flex_shrink_0()
                                    .child(
                                        if self.current_page == Page::Settings {
                                            sidebar_icon(page)
                                        } else {
                                            match page {
                                                Page::Profiles => Icon::new(
                                                    gpui_kit::assets::IconName::NotebookTabs,
                                                ),
                                                Page::Network => {
                                                    Icon::new(gpui_kit::assets::IconName::Activity)
                                                }
                                                Page::Logs => {
                                                    Icon::new(gpui_kit::assets::IconName::FileText)
                                                }
                                                _ => sidebar_icon(page),
                                            }
                                        }
                                        .size(rems(1.25))
                                        .flex_shrink_0(),
                                    ),
                            )
                            .when(!self.collapsed, |this| {
                                this.child(div().min_w_0().text_ellipsis().child(page.label()))
                            }),
                    )
                    .tooltip(page.label())
                    .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
            }))
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
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

        GpuiSidebar::new("main-sidebar")
            .w(rems(9.))
            .collapsible(true)
            .collapsed(self.collapsed)
            .header(
                h_flex().w_full().justify_end().child(
                    Button::new("toggle-sidebar")
                        .icon(toggle_icon)
                        .small()
                        .ghost()
                        .accessibility_label(toggle_label.clone())
                        .tooltip(toggle_label)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleSidebar), cx)
                        }),
                ),
            )
            .child(SidebarNavigation::new(self.current_page, navigation))
            .footer(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        SidebarNavigation::new(self.current_page, [Page::Settings])
                            .collapsed(self.collapsed),
                    )
                    .child(
                        Button::new("sidebar-appearance")
                            .w_full()
                            .h(rems(2.75))
                            .ghost()
                            .justify_start()
                            .when(self.collapsed, |this| this.justify_center().px_1())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_3()
                                    .when(self.collapsed, |this| this.justify_center())
                                    .child(
                                        div()
                                            .id("sidebar-icon:appearance")
                                            .test_support()
                                            .size(rems(1.25))
                                            .flex_shrink_0()
                                            .child(
                                                Icon::new(if dark {
                                                    IconName::Moon
                                                } else {
                                                    IconName::Sun
                                                })
                                                .size(rems(1.25))
                                                .flex_shrink_0(),
                                            ),
                                    )
                                    .when(!self.collapsed, |this| {
                                        this.child(zenclash_i18n::text(if dark {
                                            "settings.appearance.dark"
                                        } else {
                                            "settings.appearance.light"
                                        }))
                                    }),
                            )
                            .accessibility_label(zenclash_i18n::text(if dark {
                                "settings.appearance.light"
                            } else {
                                "settings.appearance.dark"
                            }))
                            .tooltip(zenclash_i18n::text(if dark {
                                "settings.appearance.light"
                            } else {
                                "settings.appearance.dark"
                            }))
                            .on_click(move |_, window, cx| {
                                if dark {
                                    window.dispatch_action(Box::new(SetLightTheme), cx);
                                } else {
                                    window.dispatch_action(Box::new(SetDarkTheme), cx);
                                }
                            }),
                    )
                    .when_some(self.status, |footer, (core, status, connected)| {
                        footer.child(
                            h_flex()
                                .border_t_1()
                                .border_color(theme.sidebar_border)
                                .px_3()
                                .py_2()
                                .gap_3()
                                .child(div().size_2p5().flex_shrink_0().rounded_full().bg(
                                    if connected {
                                        if self.current_page == Page::Settings {
                                            theme.success
                                        } else {
                                            theme.chart_3
                                        }
                                    } else {
                                        theme.muted_foreground
                                    },
                                ))
                                .when(!self.collapsed, |row| {
                                    row.child(
                                        v_flex()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .when(
                                                        self.current_page != Page::Settings
                                                            && connected,
                                                        |label| label.text_color(theme.primary),
                                                    )
                                                    .child(core),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(status),
                                            ),
                                    )
                                }),
                        )
                    }),
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

    #[gpui_kit::test]
    fn collapsed_navigation_keeps_accessible_names_and_keyboard_navigation(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::component::{Collapsible as _, Root};
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{
            AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement,
            Render, Styled, div, px, size,
        };
        struct Navigation {
            focus: FocusHandle,
            navigated: bool,
        }
        impl Render for Navigation {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div()
                    .size_full()
                    .track_focus(&self.focus)
                    .on_action(cx.listener(|this, _: &crate::app::NavigateRules, _, cx| {
                        this.navigated = true;
                        cx.notify();
                    }))
                    .child(
                        super::SidebarNavigation::new(Page::Home, [Page::Home, Page::Rules])
                            .collapsed(true),
                    )
            }
        }
        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(200.), px(200.)), |window, cx| {
            let navigation = cx.new(|cx| Navigation {
                focus: cx.focus_handle(),
                navigated: false,
            });
            view = Some(navigation.clone());
            Root::new(navigation, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            let view = view.as_ref().unwrap();
            window.render_frame(cx);
            assert_eq!(
                window.find(Page::Rules.route()).label(),
                Some(Page::Rules.label().as_str())
            );
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            for _ in 0..4 {
                if window.find(Page::Rules.route()).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(Page::Rules.route()).focused(), Some(true));
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.update(|cx| view.as_ref().unwrap().read(cx).navigated));
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
    }

    #[test]
    fn sidebar_defaults_to_expanded_and_accepts_collapsed_state() {
        assert!(!Sidebar::new(Page::Home).collapsed);
        assert!(Sidebar::new(Page::Home).collapsed(true).collapsed);
    }

    #[gpui_kit::test]
    fn collapsed_sidebar_preserves_full_icon_bounds(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, div, px, size,
        };
        struct Navigation;
        impl Render for Navigation {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut Context<Self>,
            ) -> impl IntoElement {
                div()
                    .size_full()
                    .child(Sidebar::new(Page::Home).collapsed(true))
            }
        }
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(200.), px(700.)), |window, cx| {
            let view = cx.new(|_| Navigation);
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            for page in std::iter::once(Page::Home)
                .chain(Page::PRIMARY)
                .chain([Page::Settings])
            {
                let icon = window
                    .find(format!("sidebar-icon:{}", page.route()))
                    .bounds();
                let button = window.find(page.route()).bounds();
                assert_eq!(
                    icon.size.width,
                    gpui_kit::rems(1.25).to_pixels(window.rem_size())
                );
                assert!(
                    icon.left() >= button.left() && icon.right() <= button.right(),
                    "{} icon exceeds its button",
                    page.route()
                );
            }
            window.remove_window();
        })
        .unwrap();
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
