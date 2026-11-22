//! Primary window presentation, retained visibility, shell tasks, and platform restore size.
use std::sync::Arc;

use gpui_kit::AnyWindowHandle;
#[cfg(target_os = "macos")]
use gpui_kit::{Pixels, Size, px};

use crate::shell::app::RestartRequest;
use crate::shell::app::ZenClashApp;
use gpui_kit::AppContext;
use gpui_kit::Context;

pub(in crate::shell) struct WindowShellState {
    pub(in crate::shell) main_window: AnyWindowHandle,
    pub(in crate::shell) main_window_visible: bool,
    #[cfg(target_os = "macos")]
    pub(in crate::shell) main_window_restore_size: MainWindowRestoreSize,
    pub(in crate::shell) quit_state: crate::shell::controllers::system_proxy::QuitState,
    pub(in crate::shell) restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
    pub(in crate::shell) safe_mode: zenclash_app::SafeMode,
    pub(in crate::shell) tasks: Vec<gpui_kit::Task<()>>,
}

impl WindowShellState {
    pub(in crate::shell) fn new(
        main_window: AnyWindowHandle,
        restart_after_exit: Arc<parking_lot::Mutex<Option<RestartRequest>>>,
        safe_mode: zenclash_app::SafeMode,
    ) -> Self {
        Self {
            main_window,
            main_window_visible: true,
            #[cfg(target_os = "macos")]
            main_window_restore_size: MainWindowRestoreSize::default(),
            quit_state: crate::shell::controllers::system_proxy::QuitState::default(),
            restart_after_exit,
            safe_mode,
            tasks: Vec::new(),
        }
    }
}

impl ZenClashApp {
    #[cfg(target_os = "macos")]
    pub(in crate::shell) fn park_main_window(&mut self, window: &mut gpui_kit::Window) {
        #[cfg(not(test))]
        crate::shell::platform::hide_native_window(window);
        let parked_size = self
            .shell
            .main_window_restore_size
            .park(window.bounds().size);
        window.resize(parked_size);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub(in crate::shell) fn hide_main_window_in(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.release_hidden_page_data(cx);
        self.hide_retained_window(window);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn hide_retained_window(&mut self, window: &mut gpui_kit::Window) {
        #[cfg(all(target_os = "windows", not(test)))]
        crate::shell::platform::hide_native_window(window);
        // GPUI has no Linux hide API. Keep the native window and its owner alive
        // while using the compositor's supported minimize operation (X11/Wayland).
        #[cfg(all(target_os = "linux", not(test)))]
        window.minimize_window();
        #[cfg(test)]
        let _ = window;
    }

    pub(in crate::shell) fn release_hidden_page_data(&mut self, cx: &mut Context<Self>) {
        self.shell.main_window_visible = false;
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
        self.refresh_visible_proxies(cx);
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(false, cx);
        });
    }

    #[cfg(target_os = "linux")]
    pub(in crate::shell) fn restore_main_window_after_activation(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        // A taskbar/compositor restore does not pass through the tray's show command.
        // Resume retained data without activating the window again inside its callback.
        self.shell.main_window_visible = true;
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(true, cx);
        });
        self.refresh_visible_proxies(cx);
        cx.notify();
    }

    pub(in crate::shell) fn show_main_window(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Regular);
        self.shell.main_window_visible = true;
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(true, cx);
        });
        self.refresh_visible_proxies(cx);
        #[cfg(target_os = "macos")]
        if let Some(restore_size) = self.shell.main_window_restore_size.restore() {
            let _ = cx.update_window(self.shell.main_window, move |_, window, _| {
                window.resize(restore_size);
            });
        }
        cx.activate(true);
        #[cfg(all(target_os = "windows", not(test)))]
        let _ = cx.update_window(self.shell.main_window, |_, window, _| {
            crate::shell::platform::show_native_window(window);
        });
        let _ = cx.update_window(self.shell.main_window, |_, window, _| {
            window.activate_window()
        });
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
pub(in crate::shell) struct MainWindowRestoreSize {
    restore_size: Option<Size<Pixels>>,
}

#[cfg(target_os = "macos")]
impl MainWindowRestoreSize {
    pub(in crate::shell) fn park(&mut self, current_size: Size<Pixels>) -> Size<Pixels> {
        let parked_size = gpui_kit::size(px(1.), px(1.));
        if current_size != parked_size && self.restore_size.is_none() {
            self.restore_size = Some(current_size);
        }
        parked_size
    }

    pub(in crate::shell) fn restore(&mut self) -> Option<Size<Pixels>> {
        self.restore_size.take()
    }
}

#[cfg(test)]
mod tests;
