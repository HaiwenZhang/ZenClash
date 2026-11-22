//! Application submission and observation of the shared outbound-mode capability.

use std::time::Duration;

use zenclash_app::outbound_mode::synchronize_runtime_mode;

use super::{Context, ZenClashApp};
use crate::outbound_mode::OutboundMode;

impl ZenClashApp {
    pub(in crate::app) fn request_outbound_mode(
        &mut self,
        mode: OutboundMode,
        cx: &mut Context<Self>,
    ) {
        self.proxies_page
            .update(cx, |page, cx| page.set_outbound_mode(mode.api_value(), cx));
        if self.outbound_mode.request(
            mode,
            &self.services.core_session,
            &self.services.controlled_config_store,
            &self.services.runtime,
        ) {
            let pending = self.outbound_mode.is_pending();
            self.runtime_page.update(cx, |page, cx| {
                page.begin_home_mode_transition(mode, pending, cx);
            });
            cx.notify();
        }
    }

    pub(super) fn start_mode_sync(&mut self, cx: &mut Context<Self>) {
        let session = self.services.core_session.clone();
        let runtime = self.services.runtime.clone();
        let mode = self.outbound_mode.clone();
        let logs = self.services.log_monitor.clone();
        let task = cx.spawn(async move |this, _cx| {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let intent_generation = mode.intent_generation();
                let task = runtime.spawn(synchronize_runtime_mode(
                    session.clone(),
                    mode.clone(),
                    logs.clone(),
                    intent_generation,
                ));
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "failed to synchronize Mihomo outbound mode");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "outbound mode synchronization task failed")
                    }
                }
                if this.upgrade().is_none() {
                    break;
                }
            }
        });
        self.shell.tasks.push(task);
    }
}
