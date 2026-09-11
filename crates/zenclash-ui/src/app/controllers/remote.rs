use super::ControllersPage;
use crate::{
    components::{mode::OutboundModeCoordinator, sidebar::OutboundMode},
    pages::{
        Page,
        proxies::ProxiesPage,
        runtime::{RuntimePage, RuntimePageServices},
    },
};
use gpui::{AppContext, Context, Entity, Window};
use std::sync::Arc;
use zenclash_core::{
    AppPreferences, ControlledConfigStore, ControllerEntry, CoreKind, CoreSession, LogMonitor,
    MihomoClient, MihomoLogLevel, OperationalStatus, TrafficCaptureSession, TrafficMonitor,
};

pub(super) struct RemoteWorkspace {
    pub(super) entry: ControllerEntry,
    pub(super) client: MihomoClient,
    pub(super) traffic: Arc<TrafficMonitor>,
    pub(super) mode: OutboundModeCoordinator,
    pub(super) proxies: Entity<ProxiesPage>,
    pub(super) pages: Entity<RuntimePage>,
    pub(super) page: Page,
    operational: Arc<OperationalStatus>,
}

impl RemoteWorkspace {
    pub(super) fn new(
        entry: ControllerEntry,
        client: MihomoClient,
        mode: &str,
        runtime: &tokio::runtime::Handle,
        controlled: ControlledConfigStore,
        window: &mut Window,
        cx: &mut Context<ControllersPage>,
    ) -> Self {
        let traffic = TrafficMonitor::start(runtime, entry.endpoint.clone());
        let logs = LogMonitor::start(runtime, entry.endpoint.clone(), MihomoLogLevel::Info);
        let core = CoreSession::open(CoreKind::Mihomo, client.clone(), None);
        let operational =
            OperationalStatus::start_remote(runtime, core.clone(), traffic.clone(), logs.clone());
        let capture =
            TrafficCaptureSession::new(core.clone(), controlled.clone(), None, None, None);
        let pages = cx.new(|cx| {
            RuntimePage::new_remote(
                RuntimePageServices {
                    core_kind: CoreKind::Mihomo,
                    core_session: core,
                    client: client.clone(),
                    runtime: runtime.clone(),
                    traffic_monitor: traffic.clone(),
                    log_monitor: logs,
                    operational_status: operational.clone(),
                    traffic_capture: capture,
                    process: None,
                    profile_path: None,
                    controlled_config_store: controlled,
                    preferences_store: None,
                    preferences: AppPreferences::default(),
                    system_proxy_session: None,
                    traffic_history_store: None,
                    startup_notice: None,
                    startup_error: None,
                },
                window,
                cx,
            )
        });
        pages.update(cx, |page, cx| page.set_presented(false, cx));
        let proxies = cx.new(|cx| ProxiesPage::new(client.clone(), runtime.clone(), cx));
        proxies.update(cx, |page, cx| {
            page.set_outbound_mode(mode, cx);
            page.reload(cx);
        });
        let outbound = OutboundModeCoordinator::new_unsynchronized(OutboundMode::from_api(mode));
        outbound.synchronize(OutboundMode::from_api(mode), 0);
        Self {
            entry,
            client,
            traffic,
            mode: outbound,
            proxies,
            pages,
            page: Page::Proxies,
            operational,
        }
    }

    pub(super) fn navigate(&mut self, page: Page, cx: &mut Context<ControllersPage>) {
        if !remote_page(page) {
            return;
        }
        self.page = page;
        if page == Page::Proxies {
            self.pages
                .update(cx, |page, cx| page.set_presented(false, cx));
            self.proxies.update(cx, ProxiesPage::reload);
        } else {
            self.proxies.update(cx, |page, _| page.suspend());
            self.pages.update(cx, |view, cx| {
                view.switch_to(page, cx);
                view.set_presented(true, cx);
            });
        }
        cx.notify();
    }
}

impl Drop for RemoteWorkspace {
    fn drop(&mut self) {
        // A pending status request otherwise retains both streams until its HTTP timeout.
        self.operational.stop();
    }
}

fn remote_page(page: Page) -> bool {
    matches!(
        page,
        Page::Proxies | Page::Connections | Page::Logs | Page::Rules
    )
}

impl ControllersPage {
    pub(super) fn set_remote_mode(&mut self, mode: OutboundMode, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(remote) = self.remote.as_ref() else {
            return;
        };
        let client = remote.client.clone();
        let generation = self.generation;
        self.busy = true;
        let task = self
            .runtime
            .spawn(async move { client.set_mode(mode.api_value()).await });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| {
                    result.map_err(|_| zenclash_i18n::text("controllers.command_failed"))
                });
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.busy = false;
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(remote) = &mut this.remote {
                            remote.mode.synchronize(mode, remote.mode.generation());
                            remote.proxies.update(cx, |page, cx| {
                                page.set_outbound_mode(mode.api_value(), cx)
                            });
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_navigation_cannot_reach_host_configuration_or_process_controls() {
        for page in [
            Page::Home,
            Page::Tun,
            Page::SystemProxy,
            Page::Settings,
            Page::Profiles,
            Page::Override,
            Page::Mihomo,
        ] {
            assert!(!remote_page(page));
        }
        for page in [Page::Proxies, Page::Connections, Page::Logs, Page::Rules] {
            assert!(remote_page(page));
        }
    }
}
