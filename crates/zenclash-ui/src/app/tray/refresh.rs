use super::{
    tray_directories, AppContext, Context, OutboundMode, TrayMenuState, TrayProxyGroup,
    TrayProxyNode, ZenClashApp,
};

impl ZenClashApp {
    pub(in crate::app) fn refresh_tray_menu(&mut self, cx: &mut Context<Self>) {
        if self.network_tray.is_none() {
            return;
        }
        if self.tray_refreshing {
            self.tray_refresh_pending = true;
            return;
        }
        self.tray_refreshing = true;
        self.tray_refresh_pending = false;
        let mode_generation = self.outbound_mode.generation();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let system_proxy_task = tokio::task::spawn_blocking(|| {
                zenclash_core::SystemProxyManager::detect()
                    .and_then(|manager| manager.status())
                    .map(|status| status.enabled || status.secure_enabled)
                    .map_err(|error| error.to_string())
            });
            let (config, catalog, system_proxy) = tokio::join!(
                client.runtime_config(),
                client.proxy_catalog(),
                system_proxy_task
            );
            let config = config.map_err(|error| error.to_string())?;
            let catalog = catalog.map_err(|error| error.to_string())?;
            let system_proxy = system_proxy
                .map_err(|error| format!("系统代理状态任务异常结束：{error}"))
                .and_then(|result| result);
            Ok::<_, String>((config, catalog, system_proxy, mode_generation))
        });
        let profile_path = self.profile_path.clone();

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_refreshing = false;
                match result {
                    Ok(Ok((config, catalog, system_proxy, mode_generation))) => {
                        this.apply_tray_menu_state(
                            &config,
                            catalog,
                            system_proxy,
                            mode_generation,
                            profile_path.as_deref(),
                            cx,
                        );
                    }
                    Ok(Err(error)) => tracing::warn!(%error, "failed to load tray menu state"),
                    Err(error) => tracing::warn!(%error, "tray menu state task failed"),
                }

                if this.tray_refresh_pending {
                    this.refresh_tray_menu(cx);
                    return;
                }
                if this.tray_menu_requested {
                    if let Some(tray) = this.network_tray.as_ref() {
                        tray.show_menu();
                    }
                    this.tray_menu_requested = false;
                }
            });
        })
        .detach();
    }

    fn apply_tray_menu_state(
        &mut self,
        config: &zenclash_core::RuntimeConfig,
        catalog: zenclash_core::ProxyCatalog,
        system_proxy: Result<bool, String>,
        mode_generation: u64,
        profile_path: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) {
        self.outbound_mode
            .synchronize(OutboundMode::from_api(&config.mode), mode_generation);
        let mixed_port = [config.mixed_port, config.port, config.socks_port]
            .into_iter()
            .find(|port| *port > 0)
            .unwrap_or_default();
        let groups = catalog
            .groups
            .into_iter()
            .map(|group| TrayProxyGroup {
                name: group.name,
                now: group.now,
                test_url: group.test_url,
                proxies: group
                    .all
                    .into_iter()
                    .map(|proxy| {
                        let delay = proxy.latest_delay();
                        TrayProxyNode {
                            name: proxy.name,
                            delay,
                        }
                    })
                    .collect(),
            })
            .collect();
        let profile_name = profile_path.and_then(|path| path.file_name()).map_or_else(
            || "当前配置".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        let floating_visible = self
            .floating_window
            .is_some_and(|handle| cx.update_window(handle, |_, _, _| ()).is_ok());
        if !floating_visible {
            self.floating_window = None;
        }
        let system_proxy = system_proxy.unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to read system proxy state for tray menu");
            false
        });
        let state = TrayMenuState {
            mode: self.outbound_mode.displayed().api_value().into(),
            system_proxy,
            tun: config.tun.enable,
            floating_visible,
            mixed_port,
            profile_name,
            groups,
            directories: tray_directories(profile_path),
        };
        if let Some(tray) = self.network_tray.as_mut() {
            if let Err(error) = tray.update_menu(&state) {
                tracing::warn!(%error, "failed to update tray menu");
            }
        }
        cx.notify();
    }
}
