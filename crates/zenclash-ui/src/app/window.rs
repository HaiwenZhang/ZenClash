//! Primary window presentation, retained visibility and platform restore size.
#[cfg(target_os = "macos")]
use gpui_kit::{Pixels, Size, px};

use super::{AppContext, Context, ZenClashApp};

impl ZenClashApp {
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn park_main_window(&mut self, window: &mut gpui_kit::Window) {
        #[cfg(not(test))]
        super::platform::hide_native_window(window);
        let parked_size = self.main_window_restore_size.park(window.bounds().size);
        window.resize(parked_size);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub(in crate::app) fn hide_main_window_in(
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
        super::platform::hide_native_window(window);
        // GPUI has no Linux hide API. Keep the native window and its owner alive
        // while using the compositor's supported minimize operation (X11/Wayland).
        #[cfg(all(target_os = "linux", not(test)))]
        window.minimize_window();
        #[cfg(test)]
        let _ = window;
    }

    pub(in crate::app) fn release_hidden_page_data(&mut self, cx: &mut Context<Self>) {
        self.main_window_visible = false;
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
        self.refresh_visible_proxies(cx);
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(false, cx);
        });
    }

    #[cfg(target_os = "linux")]
    pub(in crate::app) fn restore_main_window_after_activation(&mut self, cx: &mut Context<Self>) {
        // A taskbar/compositor restore does not pass through the tray's show command.
        // Resume retained data without activating the window again inside its callback.
        self.main_window_visible = true;
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(true, cx);
        });
        self.refresh_visible_proxies(cx);
        cx.notify();
    }

    pub(in crate::app) fn show_main_window(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Regular);
        self.main_window_visible = true;
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(true, cx);
        });
        self.refresh_visible_proxies(cx);
        #[cfg(target_os = "macos")]
        if let Some(restore_size) = self.main_window_restore_size.restore() {
            let _ = cx.update_window(self.main_window, move |_, window, _| {
                window.resize(restore_size);
            });
        }
        cx.activate(true);
        #[cfg(all(target_os = "windows", not(test)))]
        let _ = cx.update_window(self.main_window, |_, window, _| {
            super::platform::show_native_window(window);
        });
        let _ = cx.update_window(self.main_window, |_, window, _| window.activate_window());
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
pub(super) struct MainWindowRestoreSize {
    restore_size: Option<Size<Pixels>>,
}

#[cfg(target_os = "macos")]
impl MainWindowRestoreSize {
    pub(super) fn park(&mut self, current_size: Size<Pixels>) -> Size<Pixels> {
        let parked_size = gpui_kit::size(px(1.), px(1.));
        if current_size != parked_size && self.restore_size.is_none() {
            self.restore_size = Some(current_size);
        }
        parked_size
    }

    pub(super) fn restore(&mut self) -> Option<Size<Pixels>> {
        self.restore_size.take()
    }
}

#[cfg(test)]
mod tests;
