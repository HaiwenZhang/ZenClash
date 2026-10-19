//! Proxy commands retain the menu observation that produced each submitted intent.

use super::{Context, ZenClashApp};
use zenclash_core::{
    ConnectionPolicy, CoreObservationIdentity, CoreSession, MihomoError, ProxySelectionReadback,
};

#[derive(Clone)]
pub(in crate::app) struct TrayProxySelectionRequest {
    group: String,
    proxy: String,
    observation: CoreObservationIdentity,
}

impl TrayProxySelectionRequest {
    async fn execute(&self, session: &CoreSession) -> Result<Vec<String>, String> {
        let accepted = session
            .accept_observed_proxy_selection(
                &self.observation,
                &self.group,
                &self.proxy,
                ConnectionPolicy::KeepExisting,
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut warnings = accepted.warnings().to_vec();
        match accepted.read_actual().await {
            ProxySelectionReadback::Observed(_) => {}
            ProxySelectionReadback::Unavailable(error) => warnings.push(error.to_string()),
            ProxySelectionReadback::RuntimeChanged => {
                warnings.push(MihomoError::StaleBinding.to_string())
            }
            ProxySelectionReadback::GroupMissing => {
                warnings.push(zenclash_i18n::text("proxies.selection.group_missing"))
            }
            ProxySelectionReadback::NoCurrentMember => {
                warnings.push(zenclash_i18n::text("proxies.selection.no_current_member"))
            }
            _ => warnings.push(zenclash_i18n::text(
                "proxies.selection.observation_outdated",
            )),
        }
        Ok(warnings)
    }
}

impl ZenClashApp {
    pub(super) fn select_proxy_from_tray(
        &mut self,
        group: String,
        proxy: String,
        observation: Option<CoreObservationIdentity>,
        cx: &mut Context<Self>,
    ) {
        let Some(observation) = observation.filter(|source| source.belongs_to(&self.core_session))
        else {
            self.report_tray_error(MihomoError::StaleBinding.to_string(), cx);
            self.refresh_tray_menu(cx);
            return;
        };
        if let Some(request) = self
            .proxy_selection_commands
            .submit(TrayProxySelectionRequest {
                group,
                proxy,
                observation,
            })
        {
            self.start_proxy_selection(request, cx);
        }
    }

    fn start_proxy_selection(
        &mut self,
        request: TrayProxySelectionRequest,
        cx: &mut Context<Self>,
    ) {
        self.publish_tray_proxy_feedback(&request.observation, |this| {
            this.tray_command_error = None
        });
        let session = self.core_session.clone();
        let submitted = request.clone();
        let task = self
            .runtime
            .spawn(async move { submitted.execute(&session).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.quit_state != crate::app::system_proxy::QuitState::Idle { return; }
                match &result {
                    Ok(warnings) => for warning in warnings { tracing::warn!(%warning, "tray proxy selection completed with a warning"); },
                    Err(error) => {
                        this.publish_tray_proxy_feedback(&request.observation, |this| this.tray_command_error = Some(error.clone()));
                    }
                }
                this.refresh_visible_proxies(cx);
                if let Some(next) = this.proxy_selection_commands.complete() {
                    this.start_proxy_selection(next, cx);
                } else {
                    this.refresh_tray_menu(cx);
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn test_group_from_tray(
        &mut self,
        group: String,
        automatic: bool,
        test_url: Option<String>,
        observation: Option<CoreObservationIdentity>,
        cx: &mut Context<Self>,
    ) {
        let Some(observation) = observation.filter(|source| source.belongs_to(&self.core_session))
        else {
            self.report_tray_error(MihomoError::StaleBinding.to_string(), cx);
            self.refresh_tray_menu(cx);
            return;
        };
        let session = self.core_session.clone();
        let submitted = observation.clone();
        let task_group = group.clone();
        let task = self.runtime.spawn(async move {
            if automatic {
                session
                    .measure_observed_proxy_group(
                        &submitted,
                        &task_group,
                        test_url.as_deref(),
                        5_000,
                    )
                    .await
                    .map(|receipt| receipt.outcome().selection.warnings.clone())
                    .map_err(|error| error.to_string())
            } else {
                session
                    .measure_observed_proxy_group_delays(
                        &submitted,
                        &task_group,
                        test_url.as_deref(),
                        5_000,
                    )
                    .await
                    .map(|_| Vec::new())
                    .map_err(|error| error.to_string())
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.quit_state != crate::app::system_proxy::QuitState::Idle { return; }
                match &result {
                    Ok(warnings) => for warning in warnings { tracing::warn!(%group, %warning, "tray group delay completed with a readback warning"); },
                    Err(error) => {
                        tracing::warn!(%group, %error, "tray proxy delay test failed");
                        this.publish_tray_proxy_feedback(&observation, |this| this.tray_command_error = Some(error.clone()));
                    }
                }
                tracing::info!(%group, "tray proxy group delay test completed");
                this.refresh_visible_proxies(cx);
                this.refresh_tray_menu(cx);
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn publish_tray_proxy_feedback(
        &mut self,
        source: &CoreObservationIdentity,
        publish: impl FnOnce(&mut Self),
    ) -> bool {
        self.tray_observation
            .as_ref()
            .is_some_and(|current| current.is_same_observation(source))
            && source.publish_if_current(|| publish(self)).is_some()
    }
}

#[cfg(test)]
mod tests;
