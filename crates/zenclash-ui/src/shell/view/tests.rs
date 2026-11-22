use gpui_kit::{Element, ElementId, IntoElement};

use super::MAIN_WINDOW_CLOSE_SELECTOR;
use super::MAIN_WINDOW_MINIMIZE_SELECTOR;
use super::MAIN_WINDOW_TITLE_BAR_SELECTOR;
use super::MAIN_WINDOW_ZOOM_SELECTOR;
use super::WINDOWS_WINDOW_CONTROLS;
use super::WindowsWindowControl;
use super::main_window_title_bar;
use super::needs_client_window_controls;
use super::needs_native_window_drag;
use super::uses_custom_title_bar;

#[test]
fn controller_indicator_rejects_old_generations_and_retained_successes() {
    use super::ControllerIndicator;
    use zenclash_core::{
        ControllerCompatibility, ControllerStatus, Observation, OperationalFailure, VersionInfo,
    };
    let value = ControllerStatus {
        version: VersionInfo::default(),
        authenticated: true,
        compatibility: ControllerCompatibility::Compatible,
        generation: 7,
    };
    let fresh = Observation::Fresh {
        value: value.clone(),
        observed_at_ms: 100,
    };
    assert_eq!(
        ControllerIndicator::from_observation(&fresh, 7),
        ControllerIndicator::Connected
    );
    assert_eq!(
        ControllerIndicator::from_observation(&fresh, 8),
        ControllerIndicator::Stale
    );
    let stale = Observation::Stale {
        value,
        observed_at_ms: 100,
        failure: OperationalFailure {
            message: "controller disconnected".into(),
            occurred_at_ms: 200,
        },
    };
    assert_eq!(
        ControllerIndicator::from_observation(&stale, 7),
        ControllerIndicator::Stale
    );
    assert_eq!(
        ControllerIndicator::from_observation(&Observation::Loading, 7),
        ControllerIndicator::Loading
    );
}

#[gpui_kit::test]
fn caption_controls_have_names_and_close_through_keyboard_and_pointer(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, FocusHandle, InputEvent, InteractiveElement, MouseButton,
        MouseDownEvent, MouseUpEvent, ParentElement, Render, Styled, div, px, size,
    };
    use std::{cell::Cell, rc::Rc};

    struct Caption {
        focus: FocusHandle,
        closes: Rc<Cell<usize>>,
    }
    impl Render for Caption {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
            let closes = self.closes.clone();
            div()
                .size_full()
                .track_focus(&self.focus)
                .child(super::WindowsWindowControls {
                    on_close_window: Rc::new(move |_, _, _| closes.set(closes.get() + 1)),
                })
        }
    }
    cx.update(gpui_kit::init);
    let closes = Rc::new(Cell::new(0));
    let mut view = None;
    let window = cx.open_window(size(px(400.), px(100.)), |window, cx| {
        let caption = cx.new(|cx| Caption {
            focus: cx.focus_handle(),
            closes: closes.clone(),
        });
        view = Some(caption.clone());
        Root::new(caption, window, cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        for control in WINDOWS_WINDOW_CONTROLS {
            let target = window.find(control.selector());
            assert_eq!(target.role(), Some(gpui_kit::Role::Button));
            assert_eq!(target.label(), Some(control.label(false).as_str()));
        }
        let focus = view.as_ref().unwrap().read(cx).focus.clone();
        window.focus(&focus, cx);
        for _ in 0..5 {
            if window.find(MAIN_WINDOW_CLOSE_SELECTOR).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(
            window.find(MAIN_WINDOW_CLOSE_SELECTOR).focused(),
            Some(true)
        );
        window.press("enter", cx);
        assert_eq!(closes.get(), 1);
        window.click(MAIN_WINDOW_CLOSE_SELECTOR, cx);
        assert_eq!(closes.get(), 2);
        for selector in [MAIN_WINDOW_MINIMIZE_SELECTOR, MAIN_WINDOW_ZOOM_SELECTOR] {
            window.hover(selector, cx);
            let position = window.find(selector).bounds().center();
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            let result = window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
            assert!(
                result.propagate,
                "native caption handling must receive mouse-up"
            );
        }
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn custom_title_bar_policy_reserves_space_on_all_desktop_platforms() {
    let actual = ["windows", "linux", "macos"].map(uses_custom_title_bar);

    assert_eq!(actual, [true, true, true]);
}

#[gpui_kit::test]
fn title_bar_keeps_sidebar_and_content_below_window_controls(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::{Root, Theme};
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext, Context, InteractiveElement, ParentElement, Render, Styled, TestSupportExt,
        div, px, size,
    };

    struct Shell {
        collapsed: bool,
    }
    impl Render for Shell {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui_kit::component::v_flex()
                .size_full()
                .when(uses_custom_title_bar(std::env::consts::OS), |shell| {
                    shell.child(main_window_title_bar(|_, _, _| {}))
                })
                .child(
                    gpui_kit::component::h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_stretch()
                        .child(
                            crate::components::sidebar::Sidebar::new(
                                crate::foundation::actions::Page::Home,
                            )
                            .collapsed(self.collapsed),
                        )
                        .child(div().id("content").test_support().flex_1().min_w_0()),
                )
        }
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    for font_size in [12., 16., 20.] {
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        for window_size in [size(px(720.), px(560.)), size(px(1280.), px(820.))] {
            for collapsed in [false, true] {
                let handle = cx.open_window(window_size, |window, cx| {
                    let shell = cx.new(|_| Shell { collapsed });
                    Root::new(shell, window, cx)
                });
                cx.update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let title_bar = window.find(MAIN_WINDOW_TITLE_BAR_SELECTOR).bounds();
                    assert_eq!(title_bar.size.height, gpui_kit::component::TITLE_BAR_HEIGHT);
                    assert!(window.find("toggle-sidebar").bounds().top() >= title_bar.bottom());
                    assert!(window.find("content").bounds().top() >= title_bar.bottom());
                    window.remove_window();
                })
                .unwrap();
            }
        }
    }
}

#[test]
fn native_window_drag_region_is_windows_only() {
    let actual = ["windows", "linux", "macos"].map(needs_native_window_drag);

    assert_eq!(actual, [true, false, false]);
}

#[test]
fn client_window_controls_are_windows_only() {
    let actual = ["windows", "linux", "macos"].map(needs_client_window_controls);

    assert_eq!(actual, [true, false, false]);
}

#[test]
fn windows_window_controls_cover_all_caption_actions_in_order() {
    assert_eq!(
        WINDOWS_WINDOW_CONTROLS,
        [
            WindowsWindowControl::Minimize,
            WindowsWindowControl::Zoom,
            WindowsWindowControl::Close,
        ]
    );
    assert_eq!(
        WINDOWS_WINDOW_CONTROLS.map(WindowsWindowControl::selector),
        [
            MAIN_WINDOW_MINIMIZE_SELECTOR,
            MAIN_WINDOW_ZOOM_SELECTOR,
            MAIN_WINDOW_CLOSE_SELECTOR,
        ]
    );
}

#[test]
fn main_window_shell_builds_the_custom_title_bar() {
    let title_bar = main_window_title_bar(|_, _, _| {}).into_element();

    assert_eq!(
        title_bar.id(),
        Some(ElementId::Name(MAIN_WINDOW_TITLE_BAR_SELECTOR.into()))
    );
}

#[gpui_kit::test]
fn safe_mode_banner_renders_only_when_safe_mode_is_enabled(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, px, size};

    for enabled in [false, true] {
        let fixture = crate::test_support::RuntimePageFixture::new();
        let mut services = crate::test_support::shell_tray::app_test_services(&fixture);
        services.safe_mode = if enabled {
            zenclash_app::SafeMode::enabled()
        } else {
            zenclash_app::SafeMode::disabled()
        };
        cx.executor().allow_parking();
        cx.update(crate::shell::app::init);
        let handle = cx.open_window(size(px(1000.), px(640.)), |window, cx| {
            let preferences = services.preferences.clone();
            let app = cx.new(|cx| {
                crate::shell::app::ZenClashApp::new(
                    services,
                    None,
                    None,
                    None,
                    preferences,
                    window,
                    cx,
                )
            });
            Root::new(app, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.try_find("safe-mode-banner").is_some(), enabled);
            window.remove_window();
        })
        .unwrap();
    }
}
