use gpui_kit::component::{ActiveTheme, Root, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, TestAppContext, Window, div, px, rems, size,
};
use zenclash_ui::components::sidebar::{OutboundMode, Sidebar};
use zenclash_ui::foundation::actions::{
    NavigateConnections, NavigateHome, NavigateLogs, NavigateNetwork, NavigateProfiles,
    NavigateProxies, NavigateRules, NavigateSettings, Page, SetDarkTheme, SetLightTheme,
    ToggleSidebar,
};
use zenclash_ui::foundation::design::apply_zen_theme;

struct SidebarHarness {
    current_page: Page,
    collapsed: bool,
    theme_switches: usize,
    focus: FocusHandle,
}

impl Render for SidebarHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NavigateHome, _, cx| {
                this.current_page = Page::Home;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateProxies, _, cx| {
                this.current_page = Page::Proxies;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateProfiles, _, cx| {
                this.current_page = Page::Profiles;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateConnections, _, cx| {
                this.current_page = Page::Connections;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateRules, _, cx| {
                this.current_page = Page::Rules;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateLogs, _, cx| {
                this.current_page = Page::Logs;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateNetwork, _, cx| {
                this.current_page = Page::Network;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NavigateSettings, _, cx| {
                this.current_page = Page::Settings;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.collapsed = !this.collapsed;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SetDarkTheme, window, cx| {
                apply_zen_theme(ThemeMode::Dark, Some(window), cx);
                this.theme_switches += 1;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SetLightTheme, window, cx| {
                apply_zen_theme(ThemeMode::Light, Some(window), cx);
                this.theme_switches += 1;
                cx.notify();
            }))
            .child(Sidebar::new(self.current_page).collapsed(self.collapsed))
    }
}

#[gpui_kit::test]
fn sidebar_dispatches_navigation_and_collapse_toggle_actions(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut harness = None;
    let handle = cx.open_window(size(px(320.), px(760.)), |window, cx| {
        let view = cx.new(|cx| SidebarHarness {
            current_page: Page::Home,
            collapsed: false,
            theme_switches: 0,
            focus: cx.focus_handle(),
        });
        harness = Some(view.clone());
        Root::new(view, window, cx)
    });
    let harness = harness.unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        let focus = harness.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
        assert!(window.find(Page::Home.route()).visible());
        assert_eq!(
            window.find(Page::Home.route()).label(),
            Some(Page::Home.label().as_str())
        );
        window.click(Page::Rules.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(harness.read(cx).current_page, Page::Rules);
        assert!(window.find(Page::Rules.route()).visible());

        window.click(Page::Settings.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(harness.read(cx).current_page, Page::Settings);
        assert!(window.find(Page::Settings.route()).visible());

        window.click("toggle-sidebar", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(harness.read(cx).collapsed);

        let expected_icon_width = rems(1.25).to_pixels(window.rem_size());
        for page in std::iter::once(Page::Home)
            .chain(Page::PRIMARY)
            .chain([Page::Settings])
        {
            let button = window.find(page.route());
            assert_eq!(button.label(), Some(page.label().as_str()));
            let icon_bounds = window
                .find(format!("sidebar-icon:{}", page.route()))
                .bounds();
            assert_eq!(icon_bounds.size.width, expected_icon_width);
            assert!(icon_bounds.left() >= button.bounds().left());
            assert!(icon_bounds.right() <= button.bounds().right());
        }

        window.click(Page::Proxies.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(harness.read(cx).current_page, Page::Proxies);
        assert!(window.find(Page::Proxies.route()).visible());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn sidebar_appearance_toggle_switches_between_light_and_dark_modes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let mut harness = None;
    let handle = cx.open_window(size(px(320.), px(760.)), |window, cx| {
        let view = cx.new(|cx| SidebarHarness {
            current_page: Page::Home,
            collapsed: false,
            theme_switches: 0,
            focus: cx.focus_handle(),
        });
        harness = Some(view.clone());
        Root::new(view, window, cx)
    });
    let harness = harness.unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        let focus = harness.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("sidebar-appearance").label(),
            Some(zenclash_i18n::text("settings.appearance.dark").as_str())
        );
        window.click("sidebar-appearance", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        assert_eq!(harness.read(cx).theme_switches, 1);
        assert_eq!(
            window.find("sidebar-appearance").label(),
            Some(zenclash_i18n::text("settings.appearance.light").as_str())
        );
        window.click("sidebar-appearance", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Light);
        assert_eq!(harness.read(cx).theme_switches, 2);
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn outbound_mode_reexport_round_trips_api_values_and_labels() {
    for mode in [
        OutboundMode::Rule,
        OutboundMode::Global,
        OutboundMode::Direct,
    ] {
        assert_eq!(OutboundMode::from_api(mode.api_value()), mode);
        assert_eq!(OutboundMode::from_api(mode.code()), mode);
        assert!(!mode.label().is_empty());
        assert!(!mode.code().is_empty());
    }
    assert_eq!(OutboundMode::from_api("RULE"), OutboundMode::Rule);
    assert_eq!(OutboundMode::from_api("Global"), OutboundMode::Global);
    assert_eq!(OutboundMode::from_api("direct"), OutboundMode::Direct);
    assert_eq!(OutboundMode::from_api("unknown-mode"), OutboundMode::Rule);
}
