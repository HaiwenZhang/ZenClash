use super::{AppContext, Context, OutboundMode, Page, ZenClashApp};

impl ZenClashApp {
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn park_main_window(&mut self, window: &mut gpui_kit::Window) {
        #[cfg(not(test))]
        super::super::platform::hide_native_window(window);
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
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
        self.refresh_visible_proxies(cx);
        self.runtime_page.update(cx, |runtime_page, cx| {
            runtime_page.set_window_visible(false, cx);
        });
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
        if let Some(restore_size) = self.main_window_memory.restore() {
            let _ = cx.update_window(self.main_window, move |_, window, _| {
                window.resize(restore_size);
            });
        }
        cx.activate(true);
        let _ = cx.update_window(self.main_window, |_, window, _| window.activate_window());
    }

    pub(in crate::app) fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        let previous_page = self.current_page;
        self.current_page = page;
        if page == Page::Proxies {
            self.runtime_page
                .update(cx, |runtime_page, cx| runtime_page.set_presented(false, cx));
            self.refresh_visible_proxies(cx);
            if previous_page != page {
                let app = cx.entity().downgrade();
                cx.defer(move |cx| {
                    let _ = app.update(cx, |app, cx| {
                        if app.current_page == Page::Proxies {
                            let focus = app.focus_handle.clone();
                            let _ = cx.update_window(app.main_window, |_, window, cx| {
                                focus.focus(window, cx);
                            });
                        }
                    });
                });
            }
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
            &self.core_session,
            &self.controlled_config_store,
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
