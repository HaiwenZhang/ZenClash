use std::sync::Arc;

use gpui::{Context, Task};
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
    _update_task: Option<Task<()>>,
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
            _update_task: None,
        };
        this.start_updates(cx);
        this
    }

    fn start_updates(&mut self, cx: &mut Context<Self>) {
        let monitor = self.traffic_monitor.clone();
        let mut updates = monitor.subscribe();
        updates.mark_changed();
        self._update_task = Some(cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
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
        }));
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
