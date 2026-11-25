use crate::{
    actions::Page,
    foundation::assets::{GROUP_ICON_PATH, RADIO_ICON_PATH, RULER_ICON_PATH},
};

use super::{Sidebar, sidebar_icon_path};

#[gpui_kit::test]
fn collapsed_navigation_keeps_accessible_names_and_keyboard_navigation(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::{Collapsible as _, Root};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
        Styled, div, px, size,
    };
    struct Navigation {
        focus: FocusHandle,
        navigated: bool,
    }
    impl Render for Navigation {
        fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(cx.listener(
                    |this, _: &crate::foundation::actions::NavigateRules, _, cx| {
                        this.navigated = true;
                        cx.notify();
                    },
                ))
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
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
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

#[gpui_kit::test]
fn appearance_toggle_aligns_visible_label_and_accessible_name_in_both_modes(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::{IconName, Root, ThemeMode};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, IntoElement, ParentElement, Render, Styled, div, px, size,
    };

    struct SidebarHost;
    impl Render for SidebarHost {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(Sidebar::new(Page::Home))
        }
    }

    cx.update(gpui_kit::init);
    let handle = cx.open_window(size(px(240.), px(700.)), |window, cx| {
        let view = cx.new(|_| SidebarHost);
        Root::new(view, window, cx)
    });
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let dark = mode.is_dark();
        let (expected_icon, expected_label) = super::appearance_toggle_copy(dark);
        assert!(if dark {
            matches!(expected_icon, IconName::Sun)
        } else {
            matches!(expected_icon, IconName::Moon)
        });
        assert_eq!(
            expected_label,
            zenclash_i18n::text(if dark {
                "settings.appearance.light"
            } else {
                "settings.appearance.dark"
            })
        );
        cx.update_window(handle.into(), |_, window, cx| {
            crate::foundation::design::apply_zen_theme(mode, Some(window), cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("sidebar-appearance").label(),
                Some(expected_label.as_str())
            );
        })
        .unwrap();
    }
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
}
