use std::rc::Rc;

use super::{
    ActiveTheme, App, Context, Focusable, InteractiveElement, IntoElement, Page, ParentElement,
    Render, Sidebar, Styled, TitleBar, Window, ZenClashApp, div, h_flex, v_flex,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{ClickEvent, MouseButton, Pixels, RenderOnce, StatefulInteractiveElement as _, px};
use gpui_component::{Icon, IconName, Sizable as _};

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

                div()
                    .id(control.selector())
                    .flex()
                    .w(WINDOWS_TITLE_BAR_HEIGHT)
                    .h_full()
                    .flex_shrink_0()
                    .justify_center()
                    .content_center()
                    .items_center()
                    .occlude()
                    .text_color(cx.theme().foreground)
                    .hover(|style| style.bg(hover_background).text_color(hover_foreground))
                    .active(|style| style.bg(active_background).text_color(hover_foreground))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        match control {
                            WindowsWindowControl::Minimize => {
                                #[cfg(target_os = "windows")]
                                super::platform::minimize_active_window();
                            }
                            WindowsWindowControl::Zoom => {
                                #[cfg(target_os = "windows")]
                                super::platform::toggle_active_window_maximized();
                            }
                            WindowsWindowControl::Close => {
                                on_close_window(event, window, cx);
                            }
                        }
                    })
                    .child(Icon::new(control.icon(is_maximized)).small())
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
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            #[cfg(target_os = "windows")]
                            super::platform::start_active_window_drag();
                        }),
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
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZenClashApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.current_page {
            Page::Proxies => self.proxies_page.clone().into_any_element(),
            _ => self.runtime_page.clone().into_any_element(),
        };

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
                    .child(Sidebar::new(self.current_page).collapsed(self.sidebar_collapsed))
                    .child(div().flex_1().h_full().min_w_0().child(content)),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Element, ElementId, IntoElement};

    use super::{
        MAIN_WINDOW_CLOSE_SELECTOR, MAIN_WINDOW_MINIMIZE_SELECTOR, MAIN_WINDOW_TITLE_BAR_SELECTOR,
        MAIN_WINDOW_ZOOM_SELECTOR, WINDOWS_WINDOW_CONTROLS, WindowsWindowControl,
        main_window_title_bar, needs_client_window_controls, needs_native_window_drag,
        uses_custom_title_bar,
    };

    #[test]
    fn custom_title_bar_policy_covers_windows_and_linux_only() {
        let actual = ["windows", "linux", "macos"].map(uses_custom_title_bar);

        assert_eq!(actual, [true, true, false]);
    }

    #[test]
    fn native_window_drag_workaround_is_windows_only() {
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
