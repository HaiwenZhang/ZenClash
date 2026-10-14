use zenclash_core::{
    DiagnosticPlan, NetworkLatencyTarget, NetworkProbeRoutePreference, SupportSafe,
};

use super::{DnsCacheAction, NetworkPreferenceChange, model};
use crate::pages::runtime::{Context, Page, RuntimePage};

impl RuntimePage {
    pub(in crate::pages::runtime) fn cancel_network_diagnostics(&mut self) {
        self.network_diagnostics.query.cancel();
    }

    pub(in crate::pages::runtime) fn refresh_network_diagnostics(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Network || self.network_diagnostics.query.is_loading() {
            return;
        }
        let dns_name = self
            .network_diagnostics
            .dns_name
            .read(cx)
            .value()
            .to_string();
        let targets = model::network_latency_targets(&self.preferences.network_latency_targets);
        let plan =
            match DiagnosticPlan::new(dns_name, self.preferences.network_ip_provider, targets) {
                Ok(plan) => plan,
                Err(error) => {
                    self.network_diagnostics
                        .query
                        .reject_plan(error.to_string());
                    cx.notify();
                    return;
                }
            };
        let (request, task) = self.network_diagnostics.query.start(
            &self.runtime,
            self.core_session.clone(),
            self.operational_status.clone(),
            plan,
            self.preferences.network_probe_route,
        );
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "network.errors.probe_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.complete_network_diagnostics(request, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::pages::runtime) fn complete_network_diagnostics(
        &mut self,
        request: super::diagnostic_query::NetworkDiagnosticRequestId,
        result: Result<super::diagnostic_query::ObservedNetworkDiagnostic, String>,
        cx: &mut Context<Self>,
    ) {
        if self.page == Page::Network {
            self.network_diagnostics.query.complete(
                request,
                &self.core_session,
                &self.operational_status,
                result,
            );
            cx.notify();
        }
    }

    pub(super) fn request_network_cache_flush(
        &mut self,
        action: DnsCacheAction,
        cx: &mut Context<Self>,
    ) {
        self.network_diagnostics.cache_confirmation = Some(action);
        cx.notify();
    }

    pub(super) fn cancel_network_cache_flush(&mut self, cx: &mut Context<Self>) {
        self.network_diagnostics.cache_confirmation = None;
        cx.notify();
    }

    pub(super) fn flush_network_cache(&mut self, action: DnsCacheAction, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Network) else {
            return;
        };
        self.network_diagnostics.cache_confirmation = None;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            match action {
                DnsCacheAction::Dns => client.flush_dns_cache().await,
                DnsCacheAction::FakeIp => client.flush_fake_ip_cache().await,
            }
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "network.errors.cache_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(match action {
                            DnsCacheAction::Dns => {
                                zenclash_i18n::text("network.notices.dns_cache_flushed")
                            }
                            DnsCacheAction::FakeIp => {
                                zenclash_i18n::text("network.notices.fake_ip_cache_flushed")
                            }
                        });
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::pages::runtime) fn export_network_report(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.network_diagnostics.query.report() else {
            return;
        };
        let payload = report.export_support_bundle(SupportSafe).markdown;
        let token = self.page_task_token_for(Page::Network);
        let receiver =
            cx.prompt_for_new_path(&std::env::temp_dir(), Some("zenclash-network-report.md"));
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                result => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_page_error(token, format!("{result:?}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let task = this.update(cx, |this, _| {
                this.runtime
                    .spawn_blocking(move || std::fs::write(path, payload))
            });
            let result = match task {
                Ok(task) => task
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string())),
                Err(_) => return,
            };
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text("redesign.report_saved"))
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn add_network_latency_target(&mut self, cx: &mut Context<Self>) {
        let name = self
            .network_diagnostics
            .latency_name
            .read(cx)
            .value()
            .to_string();
        let url = self
            .network_diagnostics
            .latency_url
            .read(cx)
            .value()
            .to_string();
        match NetworkLatencyTarget::new(name, url) {
            Ok(target) => self.persist_network_preference(
                NetworkPreferenceChange::AddTarget(target),
                zenclash_i18n::text("network.notices.target_added"),
                cx,
            ),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    pub(super) fn persist_network_preference(
        &mut self,
        change: NetworkPreferenceChange,
        success: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text(
                "network.errors.preferences_unavailable",
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_scoped_mutation(
            Page::Network,
            crate::pages::runtime::busy::MutationDomain::Network,
        ) else {
            return;
        };
        let adding = matches!(&change, NetworkPreferenceChange::AddTarget(_));
        let task = self.runtime.spawn_blocking(move || {
            store
                .update(|preferences| match change {
                    NetworkPreferenceChange::Provider(provider) => {
                        preferences.network_ip_provider = provider;
                    }
                    NetworkPreferenceChange::ThroughMihomo(enabled) => {
                        preferences.network_probe_route = if enabled {
                            NetworkProbeRoutePreference::Mihomo
                        } else {
                            NetworkProbeRoutePreference::Direct
                        };
                    }
                    NetworkPreferenceChange::AddTarget(target) => {
                        preferences
                            .network_latency_targets
                            .retain(|existing| existing.url != target.url);
                        preferences.network_latency_targets.push(target);
                    }
                    NetworkPreferenceChange::RemoveTarget(url) => {
                        preferences
                            .network_latency_targets
                            .retain(|target| target.url != url);
                    }
                })
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "network.errors.preferences_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(success);
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::Network,
                            cx,
                        );
                        if adding
                            && this.is_page_task_current(token)
                            && this.network_diagnostics.adding_target
                        {
                            this.network_diagnostics.adding_target = false;
                            use gpui_kit::AppContext;
                            use gpui_kit::component::WindowExt;
                            let _ = cx.update_window(this.window_handle, |_, window, cx| {
                                window.close_dialog(cx)
                            });
                        }
                        this.cancel_network_diagnostics();
                        if this.is_page_task_current(token) {
                            this.refresh_network_diagnostics(cx);
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
