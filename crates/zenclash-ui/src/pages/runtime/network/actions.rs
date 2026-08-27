use zenclash_core::{
    DiagnosticData, DiagnosticPlan, DiagnosticReport, DiagnosticStepKind, NetworkDiagnostics,
    NetworkLatencyTarget, NetworkProbeRoutePreference, NetworkProbeSnapshot, SupportSafe,
};

use super::{DnsCacheAction, NetworkPreferenceChange, model};
use crate::pages::runtime::{
    ClipboardItem, Context, Page, PreferencesRestored, RuntimeData, RuntimePage,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn cancel_network_probe(&mut self) {
        self.network_probe.loading = false;
        self.network_probe.revision = self.network_probe.revision.wrapping_add(1);
    }

    pub(in crate::pages::runtime) fn refresh_network_probe(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Network || self.network_probe.loading {
            return;
        }
        let Some(config) = (match &self.data {
            RuntimeData::Network { config, .. } => Some(config.clone()),
            _ => None,
        }) else {
            return;
        };
        let generation = self.core_session.snapshot().generation;
        let mihomo_route = model::network_probe_route(&config, true);
        let dns_name = self.network_probe.dns_name.read(cx).value().to_string();
        let provider = self.preferences.network_ip_provider;
        let targets = model::network_latency_targets(&self.preferences.network_latency_targets);
        let mut plan = match DiagnosticPlan::new(dns_name, provider, targets) {
            Ok(plan) => plan,
            Err(error) => {
                self.network_probe.snapshot = Some(NetworkProbeSnapshot {
                    public_ip_error: Some(error.to_string()),
                    ..Default::default()
                });
                cx.notify();
                return;
            }
        };
        if let Ok(route) = mihomo_route.clone() {
            plan = plan
                .with_mihomo_route(route)
                .expect("validated Mihomo route should extend a diagnostic plan");
        }
        let selected_kind =
            if self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo {
                DiagnosticStepKind::NetworkMihomo
            } else {
                DiagnosticStepKind::NetworkDirect
            };
        self.network_probe.loading = true;
        self.network_probe.revision = self.network_probe.revision.wrapping_add(1);
        let revision = self.network_probe.revision;
        let operational_status = self.operational_status.clone();
        let diagnostics = NetworkDiagnostics::new(self.client.clone(), operational_status.clone());
        let task = self
            .runtime
            .spawn(async move { diagnostics.run(plan).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| {
                zenclash_i18n::text_with(
                    "network.errors.probe_task",
                    &[("error", error.to_string())],
                )
            });
            let _ = this.update(cx, |this, cx| {
                let path = result.as_ref().map_err(Clone::clone).and_then(|report| {
                    let route = mihomo_route.as_ref().map_err(Clone::clone)?;
                    let snapshot =
                        diagnostic_network_snapshot(report, DiagnosticStepKind::NetworkMihomo)?;
                    model::path_observation(route, generation, snapshot)
                });
                operational_status.record_path(generation, path);
                if this.page != Page::Network || this.network_probe.revision != revision {
                    return;
                }
                this.network_probe.loading = false;
                match result {
                    Ok(report) => {
                        this.network_probe.snapshot = Some(
                            diagnostic_network_snapshot(&report, selected_kind)
                                .cloned()
                                .unwrap_or_else(|error| NetworkProbeSnapshot {
                                    public_ip_error: Some(error),
                                    ..NetworkProbeSnapshot::default()
                                }),
                        );
                        this.network_probe.report = Some(report);
                    }
                    Err(error) => {
                        this.network_probe.snapshot = Some(NetworkProbeSnapshot {
                            public_ip_error: Some(error),
                            ..Default::default()
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn request_network_cache_flush(
        &mut self,
        action: DnsCacheAction,
        cx: &mut Context<Self>,
    ) {
        self.network_probe.cache_confirmation = Some(action);
        cx.notify();
    }

    pub(super) fn cancel_network_cache_flush(&mut self, cx: &mut Context<Self>) {
        self.network_probe.cache_confirmation = None;
        cx.notify();
    }

    pub(super) fn flush_network_cache(&mut self, action: DnsCacheAction, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Network) else {
            return;
        };
        self.network_probe.cache_confirmation = None;
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
                this.mutating = false;
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

    pub(super) fn copy_network_support_bundle(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.network_probe.report.as_ref() else {
            return;
        };
        let diagnostics =
            NetworkDiagnostics::new(self.client.clone(), self.operational_status.clone());
        let bundle = diagnostics.export(report, SupportSafe);
        cx.write_to_clipboard(ClipboardItem::new_string(bundle.json));
        self.notice = Some(zenclash_i18n::text("network.notices.support_bundle_copied"));
        cx.notify();
    }

    pub(super) fn add_network_latency_target(&mut self, cx: &mut Context<Self>) {
        let name = self.network_probe.latency_name.read(cx).value().to_string();
        let url = self.network_probe.latency_url.read(cx).value().to_string();
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
        let Some(token) = self.begin_mutation(Page::Network) else {
            return;
        };
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
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        this.preferences = preferences.clone();
                        this.notice = Some(success);
                        cx.emit(PreferencesRestored { preferences });
                        this.cancel_network_probe();
                        this.refresh_network_probe(cx);
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn diagnostic_network_snapshot(
    report: &DiagnosticReport,
    kind: DiagnosticStepKind,
) -> Result<&NetworkProbeSnapshot, String> {
    let step = report
        .step(kind)
        .ok_or_else(|| zenclash_i18n::text("network.errors.missing_diagnostic_step"))?;
    match &step.outcome {
        Ok(DiagnosticData::Network(snapshot)) => Ok(snapshot),
        Ok(_) => Err(zenclash_i18n::text(
            "network.errors.invalid_diagnostic_step",
        )),
        Err(error) => Err(error.message.clone()),
    }
}
