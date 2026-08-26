use zenclash_core::{
    NetworkLatencyTarget, NetworkProbeRoutePreference, NetworkProbeService, NetworkProbeSnapshot,
};

use super::{model, NetworkPreferenceChange};
use crate::pages::runtime::{Context, Page, PreferencesRestored, RuntimeData, RuntimePage};

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
        let route = match model::network_probe_route(
            &config,
            self.preferences.network_probe_route == NetworkProbeRoutePreference::Mihomo,
        ) {
            Ok(route) => route,
            Err(error) => {
                self.network_probe.snapshot = Some(NetworkProbeSnapshot {
                    route: "内核未监听 HTTP/Mixed 端口".into(),
                    public_ip_error: Some(error),
                    ..Default::default()
                });
                cx.notify();
                return;
            }
        };
        let service = match NetworkProbeService::new(route) {
            Ok(service) => service,
            Err(error) => {
                self.network_probe.snapshot = Some(NetworkProbeSnapshot {
                    public_ip_error: Some(error.to_string()),
                    ..Default::default()
                });
                cx.notify();
                return;
            }
        };
        let provider = self.preferences.network_ip_provider;
        let targets = model::network_latency_targets(&self.preferences.network_latency_targets);
        self.network_probe.loading = true;
        self.network_probe.revision = self.network_probe.revision.wrapping_add(1);
        let revision = self.network_probe.revision;
        let task = self
            .runtime
            .spawn(async move { service.snapshot(provider, &targets).await });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("网络探测任务异常结束：{error}"))
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if this.page != Page::Network || this.network_probe.revision != revision {
                    return;
                }
                this.network_probe.loading = false;
                match result {
                    Ok(snapshot) => this.network_probe.snapshot = Some(snapshot),
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

    pub(super) fn add_network_latency_target(&mut self, cx: &mut Context<Self>) {
        let name = self.network_latency_name.read(cx).value().to_string();
        let url = self.network_latency_url.read(cx).value().to_string();
        match NetworkLatencyTarget::new(name, url) {
            Ok(target) => self.persist_network_preference(
                NetworkPreferenceChange::AddTarget(target),
                "自定义延迟目标已保存",
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
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some("应用设置存储不可用；请检查应用数据目录权限".into());
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
                .map_err(|error| format!("网络探测设置保存任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        this.preferences = preferences.clone();
                        this.notice = Some(success.into());
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
