use super::{
    AppContext, Context, FloatingTrafficWindow, OutboundMode, Page, Root, TitleBar, WindowBounds,
    WindowKind, WindowOptions, ZenClashApp, px,
};

impl ZenClashApp {
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn park_main_window(&mut self, window: &mut gpui::Window) {
        let parked_size = self.main_window_memory.park(window.bounds().size);
        window.resize(parked_size);
    }

    pub(super) fn hide_main_window(&mut self, cx: &mut Context<Self>) {
        self.release_hidden_page_data(cx);
        #[cfg(target_os = "macos")]
        {
            let main_window = self.main_window;
            let _ = cx.update_window(main_window, |_, window, _| self.park_main_window(window));
        }
        cx.hide();
    }

    pub(in crate::app) fn release_hidden_page_data(&mut self, cx: &mut Context<Self>) {
        self.main_window_visible = false;
        self.refresh_visible_proxies(cx);
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(false, cx);
        });
    }

    pub(super) fn show_main_window(&mut self, cx: &mut Context<Self>) {
        self.main_window_visible = true;
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(true, cx);
        });
        self.refresh_visible_proxies(cx);
        #[cfg(target_os = "macos")]
        if let Some(restore_size) = self.main_window_memory.restore() {
            let _ = cx.update_window(self.main_window, move |_, window, _| {
                window.resize(restore_size);
            });
        }
        cx.activate(true);
        let _ = cx.update_window(self.main_window, |_, window, _| window.activate_window());
    }

    pub(in crate::app) fn toggle_floating_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.floating_window.take()
            && cx
                .update_window(handle, |_, window, _| window.remove_window())
                .is_ok()
        {
            self.refresh_tray_menu(cx);
            return;
        }

        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let traffic_monitor = self.traffic_monitor.clone();
        let outbound_mode = self.outbound_mode.clone();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(gpui::size(px(390.), px(230.)), cx)),
            titlebar: Some(TitleBar::title_bar_options()),
            kind: WindowKind::Floating,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };
        match cx.open_window(options, |window, cx| {
            window.set_window_title("ZenClash Signal");
            let floating = cx.new(|cx| {
                FloatingTrafficWindow::new(client, runtime, traffic_monitor, outbound_mode, cx)
            });
            cx.new(|cx| Root::new(floating, window, cx))
        }) {
            Ok(handle) => self.floating_window = Some(handle.into()),
            Err(error) => tracing::warn!(%error, "failed to open floating traffic window"),
        }
        self.refresh_tray_menu(cx);
    }

    pub(in crate::app) fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        let previous_page = self.current_page;
        self.current_page = page;
        if page == Page::Proxies {
            self.runtime_page
                .update(cx, |runtime_page, cx| runtime_page.set_presented(false, cx));
            self.refresh_visible_proxies(cx);
        } else {
            if previous_page == Page::Proxies {
                self.refresh_visible_proxies(cx);
            }
            self.runtime_page.update(cx, |runtime_page, cx| {
                runtime_page.switch_to(page, cx);
                runtime_page.set_presented(true, cx);
            });
        }
        cx.notify();
    }

    pub(in crate::app) fn refresh_visible_proxies(&mut self, cx: &mut Context<Self>) {
        if self.current_page == Page::Proxies && self.main_window_visible {
            self.proxies_page
                .update(cx, crate::pages::proxies::ProxiesPage::reload);
        } else {
            self.proxies_page.update(cx, |page, _| page.suspend());
        }
    }

    pub(in crate::app) fn set_mode(&mut self, mode: OutboundMode, cx: &mut Context<Self>) {
        self.proxies_page
            .update(cx, |page, cx| page.set_outbound_mode(mode.api_value(), cx));
        if self.outbound_mode.request(
            mode,
            &self.client,
            self.profile_path
                .clone()
                .map(|profile| (self.controlled_config_store.clone(), profile)),
            &self.runtime,
        ) {
            let pending = self.outbound_mode.is_pending();
            self.runtime_page.update(cx, |page, cx| {
                page.begin_home_mode_transition(mode, pending, cx);
            });
            cx.notify();
        }
    }
}
