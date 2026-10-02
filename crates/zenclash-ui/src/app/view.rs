use std::rc::Rc;

use super::{
    ActiveTheme, App, Context, Focusable, InteractiveElement, IntoElement, Page, ParentElement,
    Render, Sidebar, Styled, TitleBar, Window, ZenClashApp, div, h_flex, v_flex,
};
use gpui_kit::component::{
    IconName,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{ClickEvent, Pixels, RenderOnce, WindowControlArea, px};

const MAIN_WINDOW_TITLE_BAR_SELECTOR: &str = "main-window-title-bar";
const MAIN_WINDOW_DRAG_SELECTOR: &str = "main-window-drag-area";
const MAIN_WINDOW_MINIMIZE_SELECTOR: &str = "main-window-minimize";
const MAIN_WINDOW_ZOOM_SELECTOR: &str = "main-window-zoom";
const MAIN_WINDOW_CLOSE_SELECTOR: &str = "main-window-close";
const WINDOWS_TITLE_BAR_HEIGHT: Pixels = px(34.);

type WindowCloseListener = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowsWindowControl {
    Minimize,
    Zoom,
    Close,
}

const WINDOWS_WINDOW_CONTROLS: [WindowsWindowControl; 3] = [
    WindowsWindowControl::Minimize,
    WindowsWindowControl::Zoom,
    WindowsWindowControl::Close,
];

impl WindowsWindowControl {
    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Zoom => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Minimize => MAIN_WINDOW_MINIMIZE_SELECTOR,
            Self::Zoom => MAIN_WINDOW_ZOOM_SELECTOR,
            Self::Close => MAIN_WINDOW_CLOSE_SELECTOR,
        }
    }

    fn icon(self, is_maximized: bool) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Zoom if is_maximized => IconName::WindowRestore,
            Self::Zoom => IconName::WindowMaximize,
            Self::Close => IconName::WindowClose,
        }
    }

    fn is_close(self) -> bool {
        self == Self::Close
    }

    fn label(self, is_maximized: bool) -> String {
        zenclash_i18n::text(match self {
            Self::Minimize => "app.window.minimize",
            Self::Zoom if is_maximized => "app.window.restore",
            Self::Zoom => "app.window.maximize",
            Self::Close => "app.window.close",
        })
    }
}

#[derive(IntoElement)]
struct WindowsWindowControls {
    on_close_window: WindowCloseListener,
}

impl RenderOnce for WindowsWindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_maximized = window.is_maximized();

        h_flex()
            .id("main-window-controls")
            .absolute()
            .top_0()
            .right_0()
            .h(WINDOWS_TITLE_BAR_HEIGHT)
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .children(WINDOWS_WINDOW_CONTROLS.map(|control| {
                let hover_foreground = if control.is_close() {
                    cx.theme().danger_foreground
                } else {
                    cx.theme().secondary_foreground
                };
                let hover_background = if control.is_close() {
                    cx.theme().danger
                } else {
                    cx.theme().secondary_hover
                };
                let active_background = if control.is_close() {
                    cx.theme().danger_active
                } else {
                    cx.theme().secondary_active
                };
                let on_close_window = self.on_close_window.clone();

                Button::new(control.selector())
                    .window_control_area(control.area())
                    .icon(control.icon(is_maximized))
                    .accessibility_label(control.label(is_maximized))
                    .tooltip(control.label(is_maximized))
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .foreground(hover_foreground)
                            .hover(hover_background)
                            .active(active_background),
                    )
                    .rounded_none()
                    .p_0()
                    .flex()
                    .w(WINDOWS_TITLE_BAR_HEIGHT)
                    .h_full()
                    .flex_shrink_0()
                    .justify_center()
                    .content_center()
                    .items_center()
                    .occlude()
                    .text_color(cx.theme().foreground)
                    .on_click(move |event, window, cx| {
                        // Let GPUI's native caption handling toggle minimize/maximize.
                        // Its Windows `zoom_window` API only maximizes; consuming the
                        // mouse event would prevent the native restore operation.
                        if matches!(event, ClickEvent::Mouse(_))
                            && control != WindowsWindowControl::Close
                        {
                            return;
                        }
                        cx.stop_propagation();
                        match control {
                            WindowsWindowControl::Minimize => {
                                window.minimize_window();
                            }
                            WindowsWindowControl::Zoom => {
                                #[cfg(target_os = "windows")]
                                super::platform::toggle_window_maximized(window);
                                #[cfg(not(target_os = "windows"))]
                                window.zoom_window();
                            }
                            WindowsWindowControl::Close => {
                                on_close_window(event, window, cx);
                            }
                        }
                    })
            }))
    }
}

fn uses_custom_title_bar(target_os: &str) -> bool {
    matches!(target_os, "windows" | "linux")
}

fn needs_native_window_drag(target_os: &str) -> bool {
    target_os == "windows"
}

fn needs_client_window_controls(target_os: &str) -> bool {
    target_os == "windows"
}

fn main_window_title_bar(
    on_close_window: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_close_window: WindowCloseListener = Rc::new(on_close_window);
    let linux_close_listener = on_close_window.clone();
    let title_bar = TitleBar::new()
        .on_close_window(move |event, window, cx| {
            linux_close_listener(event, window, cx);
        })
        .when(
            needs_native_window_drag(std::env::consts::OS),
            |title_bar| {
                title_bar.child(
                    div()
                        .id(MAIN_WINDOW_DRAG_SELECTOR)
                        .flex_1()
                        .h_full()
                        .window_control_area(WindowControlArea::Drag),
                )
            },
        );

    div()
        .id(MAIN_WINDOW_TITLE_BAR_SELECTOR)
        .relative()
        .flex_shrink_0()
        .child(title_bar)
        .when(
            needs_client_window_controls(std::env::consts::OS),
            |title_bar| title_bar.child(WindowsWindowControls { on_close_window }),
        )
}

impl Focusable for ZenClashApp {
    fn focus_handle(&self, _: &App) -> gpui_kit::FocusHandle {
        self.focus_handle.clone()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ControllerIndicator {
    Connected,
    #[default]
    Loading,
    Stale,
    Unavailable,
}

impl ControllerIndicator {
    pub(super) fn from_observation(
        observation: &zenclash_core::Observation<zenclash_core::ControllerStatus>,
        generation: u64,
    ) -> Self {
        match observation {
            zenclash_core::Observation::Fresh { value, .. }
                if value.authenticated && value.generation == generation =>
            {
                Self::Connected
            }
            zenclash_core::Observation::Loading => Self::Loading,
            zenclash_core::Observation::Stale { .. } => Self::Stale,
            zenclash_core::Observation::Fresh { value, .. } if value.authenticated => Self::Stale,
            _ => Self::Unavailable,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Connected => "app.status.controller_connected",
            Self::Loading => "app.status.controller_loading",
            Self::Stale => "app.status.controller_stale",
            Self::Unavailable => "app.status.controller_unavailable",
        }
    }
}

impl Render for ZenClashApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.current_page {
            Page::Proxies => self.proxies_page.clone().into_any_element(),
            _ => self.runtime_page.clone().into_any_element(),
        };
        let connected = self.controller_indicator == ControllerIndicator::Connected;
        let status_key = self.controller_indicator.key();

        v_flex()
            .id("zenclash-app")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("ZenClash")
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_navigate_home))
            .on_action(cx.listener(Self::on_navigate_system_proxy))
            .on_action(cx.listener(Self::on_navigate_tun))
            .on_action(cx.listener(Self::on_navigate_profiles))
            .on_action(cx.listener(Self::on_navigate_proxies))
            .on_action(cx.listener(Self::on_navigate_mihomo))
            .on_action(cx.listener(Self::on_navigate_connections))
            .on_action(cx.listener(Self::on_navigate_dns))
            .on_action(cx.listener(Self::on_navigate_sniffer))
            .on_action(cx.listener(Self::on_navigate_logs))
            .on_action(cx.listener(Self::on_navigate_rules))
            .on_action(cx.listener(Self::on_navigate_resources))
            .on_action(cx.listener(Self::on_navigate_override))
            .on_action(cx.listener(Self::on_navigate_network))
            .on_action(cx.listener(Self::on_navigate_traffic))
            .on_action(cx.listener(Self::on_navigate_settings))
            .on_action(cx.listener(Self::on_set_rule_mode))
            .on_action(cx.listener(Self::on_set_global_mode))
            .on_action(cx.listener(Self::on_set_direct_mode))
            .on_action(cx.listener(Self::on_set_system_theme))
            .on_action(cx.listener(Self::on_set_light_theme))
            .on_action(cx.listener(Self::on_set_dark_theme))
            .on_action(cx.listener(Self::on_show_traffic_icon))
            .on_action(cx.listener(Self::on_hide_traffic_icon))
            .on_action(cx.listener(Self::on_show_status_menu))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_floating_window))
            .when(uses_custom_title_bar(std::env::consts::OS), |shell| {
                shell.child(main_window_title_bar(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.begin_quit(None, cx)),
                ))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .child(Sidebar::new(self.current_page).collapsed(self.sidebar_collapsed))
                    .child(div().flex_1().h_full().min_w_0().child(content)),
            )
            .child(
                h_flex()
                    .id("main-window-status-bar")
                    .min_h_6()
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().size_2().rounded_full().bg(if connected {
                                theme.success
                            } else {
                                theme.muted_foreground
                            }))
                            .child(zenclash_i18n::text(status_key)),
                    )
                    .child(format!("ZenClash {}", env!("ZENCLASH_BUILD_VERSION"))),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Element, ElementId, IntoElement};

    use super::{
        MAIN_WINDOW_CLOSE_SELECTOR, MAIN_WINDOW_MINIMIZE_SELECTOR, MAIN_WINDOW_TITLE_BAR_SELECTOR,
        MAIN_WINDOW_ZOOM_SELECTOR, WINDOWS_WINDOW_CONTROLS, WindowsWindowControl,
        main_window_title_bar, needs_client_window_controls, needs_native_window_drag,
        uses_custom_title_bar,
    };

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
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut Context<Self>,
            ) -> impl IntoElement {
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
    fn custom_title_bar_policy_covers_windows_and_linux_only() {
        let actual = ["windows", "linux", "macos"].map(uses_custom_title_bar);

        assert_eq!(actual, [true, true, false]);
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
}
