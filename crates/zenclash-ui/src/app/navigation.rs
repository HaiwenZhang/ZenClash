//! Application page navigation and the shared Page-to-Action translation.
use super::action_types::*;
use super::{App, AppContext, Context, Page, Window, ZenClashApp};

impl ZenClashApp {
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
}

pub(crate) fn dispatch_navigate(page: Page, window: &mut Window, cx: &mut App) {
    match page {
        Page::Home => window.dispatch_action(Box::new(NavigateHome), cx),
        Page::SystemProxy => window.dispatch_action(Box::new(NavigateSystemProxy), cx),
        Page::Tun => window.dispatch_action(Box::new(NavigateTun), cx),
        Page::Profiles => window.dispatch_action(Box::new(NavigateProfiles), cx),
        Page::Proxies => window.dispatch_action(Box::new(NavigateProxies), cx),
        Page::Mihomo => window.dispatch_action(Box::new(NavigateMihomo), cx),
        Page::Connections => window.dispatch_action(Box::new(NavigateConnections), cx),
        Page::Dns => window.dispatch_action(Box::new(NavigateDns), cx),
        Page::Sniffer => window.dispatch_action(Box::new(NavigateSniffer), cx),
        Page::Logs => window.dispatch_action(Box::new(NavigateLogs), cx),
        Page::Rules => window.dispatch_action(Box::new(NavigateRules), cx),
        Page::Resources => window.dispatch_action(Box::new(NavigateResources), cx),
        Page::Override => window.dispatch_action(Box::new(NavigateOverride), cx),
        Page::Network => window.dispatch_action(Box::new(NavigateNetwork), cx),
        Page::Traffic => window.dispatch_action(Box::new(NavigateTraffic), cx),
        Page::Settings => window.dispatch_action(Box::new(NavigateSettings), cx),
    }
}
