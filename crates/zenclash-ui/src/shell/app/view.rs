use std::rc::Rc;

use crate::components::sidebar::Sidebar;
use crate::foundation::actions::Page;
use crate::shell::app::ZenClashApp;
use gpui_kit::App;
use gpui_kit::Context;
use gpui_kit::Focusable;
use gpui_kit::InteractiveElement;
use gpui_kit::IntoElement;
use gpui_kit::ParentElement;
use gpui_kit::Render;
use gpui_kit::Styled;
use gpui_kit::Window;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::TitleBar;
use gpui_kit::component::h_flex;
use gpui_kit::component::v_flex;
use gpui_kit::component::{
    Disableable, IconName, Sizable,
    alert::Alert,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
};
use gpui_kit::div;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Pixels, RenderOnce, StatefulInteractiveElement, TestSupportExt, WindowControlArea,
    px,
};

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
                                crate::shell::platform::toggle_window_maximized(window);
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
    matches!(target_os, "windows" | "linux" | "macos")
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
        .when(!cfg!(target_os = "macos"), |title_bar| {
            title_bar.child(h_flex().px_3().child("ZenClash"))
        })
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
        .test_support()
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
pub(in crate::shell) enum ControllerIndicator {
    Connected,
    #[default]
    Loading,
    Stale,
    Unavailable,
}

impl ControllerIndicator {
    pub(in crate::shell) fn from_observation(
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.current_page {
            Page::Proxies => self.proxies_page.clone().into_any_element(),
            _ => self.runtime_page.clone().into_any_element(),
        };
        let connected = self.controller_indicator == ControllerIndicator::Connected;
        let status_key = self.controller_indicator.key();
        let operational = self.services.operational_status.snapshot();
        let process = operational
            .process
            .is_fresh()
            .then(|| operational.process.value())
            .flatten()
            .filter(|process| process.generation == self.services.core_session.generation());
        let (sidebar_core, sidebar_detail, sidebar_live) = if self.current_page == Page::Settings {
            (
                self.services.core_kind.display_name().to_owned(),
                zenclash_i18n::text(status_key),
                connected,
            )
        } else {
            let live = process.is_some_and(|process| process.running);
            let state = match process {
                Some(process) if process.running => "app.status.running",
                Some(process)
                    if process.recovery == zenclash_core::ProcessRecoveryStatus::Stopped =>
                {
                    "home.evidence.core_stopped"
                }
                Some(_) => "home.evidence.core_unavailable",
                None => "common.status.unknown",
            };
            let detail = if connected {
                crate::components::sidebar::runtime_uptime(process)
                    .unwrap_or_else(|| zenclash_i18n::text(status_key))
            } else {
                zenclash_i18n::text(status_key)
            };
            (
                format!(
                    "{} · {}",
                    self.services.core_kind.display_name(),
                    zenclash_i18n::text(state)
                ),
                detail,
                live,
            )
        };
        let safe_mode_enabled = self.shell.safe_mode.is_enabled();

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
            .when(uses_custom_title_bar(std::env::consts::OS), |shell| {
                shell.child(main_window_title_bar(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        #[cfg(target_os = "macos")]
                        {
                            let _ = window;
                            this.begin_quit(None, cx);
                        }
                        #[cfg(any(target_os = "windows", target_os = "linux"))]
                        this.hide_main_window_in(window, cx);
                    },
                )))
            })
            .when(safe_mode_enabled, |shell| {
                shell.child(safe_mode_banner(&theme))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .child(
                        Sidebar::new(self.current_page)
                            .collapsed(
                                self.sidebar_collapsed
                                    || window.viewport_size().width < window.rem_size() * 68.,
                            )
                            .status(sidebar_core, sidebar_detail, sidebar_live),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .when_some(
                                self.preferences.menu_saves.failure_message(),
                                |view, message| {
                                    view.child(
                                        h_flex()
                                            .id("menu-preference-save-failure")
                                            .test_support()
                                            .flex_shrink_0()
                                            .items_center()
                                            .gap_3()
                                            .px_4()
                                            .py_2()
                                            .child(
                                                Alert::warning(
                                                    "menu-preference-save-alert",
                                                    message,
                                                )
                                                .small()
                                                .flex_1()
                                                .min_w_0(),
                                            )
                                            .child(
                                                Button::new("retry-menu-preferences")
                                                    .label(zenclash_i18n::text(
                                                        "common.actions.retry",
                                                    ))
                                                    .small()
                                                    .outline()
                                                    .disabled(
                                                        self.preferences
                                                            .menu_saves
                                                            .has_pending_retry(),
                                                    )
                                                    .loading(
                                                        self.preferences
                                                            .menu_saves
                                                            .has_pending_retry(),
                                                    )
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.retry_menu_preferences(cx)
                                                    })),
                                            ),
                                    )
                                },
                            )
                            .when_some(self.tray.feedback_error(), |view, message| {
                                view.child(
                                    div()
                                        .id("tray-feedback")
                                        .test_support()
                                        .aria_label(message.clone())
                                        .flex_shrink_0()
                                        .px_4()
                                        .py_2()
                                        .child(Alert::error("tray-error-alert", message).small()),
                                )
                            })
                            .child(div().flex_1().min_h_0().child(content))
                            .when(self.current_page == Page::Settings, |this| {
                                this.child(
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
                                                .child(div().size_2().rounded_full().bg(
                                                    if connected {
                                                        theme.success
                                                    } else {
                                                        theme.muted_foreground
                                                    },
                                                ))
                                                .child(zenclash_i18n::text(status_key)),
                                        )
                                        .child(format!(
                                            "ZenClash {}",
                                            env!("ZENCLASH_BUILD_VERSION")
                                        )),
                                )
                            }),
                    ),
            )
    }
}

fn safe_mode_banner(theme: &gpui_kit::component::Theme) -> impl IntoElement {
    h_flex()
        .id("safe-mode-banner")
        .test_support()
        .role(gpui_kit::Role::Status)
        .aria_label("SAFE MODE")
        .flex_shrink_0()
        .w_full()
        .px_4()
        .py_1p5()
        .gap_2()
        .items_center()
        .justify_center()
        .bg(theme.warning)
        .text_color(theme.warning_foreground)
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(gpui_kit::component::Icon::new(IconName::TriangleAlert).size_3p5())
        .child("SAFE MODE")
}

#[cfg(test)]
mod tests;
