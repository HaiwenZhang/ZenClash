use std::{sync::Arc, time::Duration};

use gpui::Context;
use zenclash_core::{MihomoClient, TrafficMonitor, TrafficSnapshot};

use super::{mode::OutboundModeCoordinator, sidebar::OutboundMode};

mod view;

/// Compact floating window showing traffic and outbound-mode controls.
pub struct FloatingTrafficWindow {
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    traffic: TrafficSnapshot,
    outbound_mode: OutboundModeCoordinator,
}

impl FloatingTrafficWindow {
    /// Creates the floating monitor and starts its periodic state synchronization.
    pub(crate) fn new(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        traffic_monitor: Arc<TrafficMonitor>,
        outbound_mode: OutboundModeCoordinator,
        cx: &mut Context<Self>,
    ) -> Self {
        let traffic = traffic_monitor.snapshot();
        let mut this = Self {
            client,
            runtime,
            traffic_monitor,
            traffic,
            outbound_mode,
        };
        this.start_updates(cx);
        this
    }

    fn start_updates(&mut self, cx: &mut Context<Self>) {
        let monitor = self.traffic_monitor.clone();
        let mut revision = monitor.revision();
        cx.spawn(async move |this, cx| {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let current_revision = monitor.revision();
                if current_revision == revision {
                    continue;
                }
                revision = current_revision;
                let traffic = monitor.snapshot();
                if this
                    .update(cx, |this, cx| {
                        this.traffic = traffic;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_mode(&mut self, mode: OutboundMode, cx: &mut Context<Self>) {
        if self
            .outbound_mode
            .request(mode, &self.client, None, &self.runtime)
        {
            cx.notify();
        }
    }
}
