//! Application submission and observation of the shared outbound-mode capability.
use super::{Context, MihomoLogLevel, ZenClashApp};
use crate::outbound_mode::OutboundMode;
use std::time::Duration;

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
    pub(super) fn start_mode_sync(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let mode = self.outbound_mode.clone();
        let logs = self.log_monitor.clone();
        cx.spawn(async move |this, _cx| {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let intent_generation = mode.intent_generation();
                let client = client.clone();
                let task = runtime.spawn(async move { client.runtime_config().await });
                match task.await {
                    Ok(Ok(config)) => {
                        mode.synchronize(OutboundMode::from_api(&config.mode), intent_generation);
                        if let Some(level) = MihomoLogLevel::from_api(&config.log_level) {
                            logs.set_level(level);
                        }
                    }
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
        })
        .detach();
    }
}
