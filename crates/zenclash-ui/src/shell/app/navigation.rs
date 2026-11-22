//! Application page navigation and the shared Page-to-Action translation.
use crate::foundation::actions::Page;
use crate::shell::app::ZenClashApp;
use gpui_kit::AppContext;
use gpui_kit::Context;

impl ZenClashApp {
    pub(in crate::shell) fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
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
                            let _ = cx.update_window(app.shell.main_window, |_, window, cx| {
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

    pub(in crate::shell) fn refresh_visible_proxies(&mut self, cx: &mut Context<Self>) {
        if self.current_page == Page::Proxies && self.shell.main_window_visible {
            self.proxies_page
                .update(cx, crate::features::proxies::ProxiesPage::reload);
        } else {
            self.proxies_page.update(cx, |page, _| page.suspend());
        }
    }
}
