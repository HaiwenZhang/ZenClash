use super::{
    tray_directories, AppContext, Context, OutboundMode, TrayMenuState, TrayProfile,
    TrayProxyGroup, TrayProxyNode, ZenClashApp,
};

struct TrayMenuSnapshot {
    config: zenclash_core::RuntimeConfig,
    catalog: zenclash_core::ProxyCatalog,
    system_proxy: Result<bool, String>,
    profiles: Result<zenclash_core::ProfileCatalog, String>,
    mode_generation: u64,
    profile_path: Option<std::path::PathBuf>,
}

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
                    .map(|status| status.active())
                    .map_err(|error| error.to_string())
            });
            let profile_catalog_task = tokio::task::spawn_blocking(|| {
                let store =
                    zenclash_core::ProfileStore::discover().map_err(|error| error.to_string())?;
                store.load().map_err(|error| error.to_string())
            });
            let (config, catalog, system_proxy, profiles) = tokio::join!(
                client.runtime_config(),
                client.proxy_catalog(),
                system_proxy_task,
                profile_catalog_task
            );
            let config = config.map_err(|error| error.to_string())?;
            let catalog = catalog.map_err(|error| error.to_string())?;
            let system_proxy = system_proxy
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "tray.errors.system_proxy_status",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let profiles = profiles
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "tray.errors.config_directory",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            Ok::<_, String>((config, catalog, system_proxy, profiles, mode_generation))
        });
        let profile_path = self.profile_path.clone();

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_refreshing = false;
                match result {
                    Ok(Ok((config, catalog, system_proxy, profiles, mode_generation))) => {
                        this.apply_tray_menu_state(
                            TrayMenuSnapshot {
                                config,
                                catalog,
                                system_proxy,
                                profiles,
                                mode_generation,
                                profile_path,
                            },
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

    fn apply_tray_menu_state(&mut self, snapshot: TrayMenuSnapshot, cx: &mut Context<Self>) {
        let TrayMenuSnapshot {
            config,
            catalog,
            system_proxy,
            profiles: profile_catalog,
            mode_generation,
            profile_path,
        } = snapshot;
        self.outbound_mode
            .synchronize(OutboundMode::from_api(&config.mode), mode_generation);
        let mixed_port = config.system_proxy_port().unwrap_or_default();
        let outbound_mode = config.mode.clone();
        let groups = catalog
            .into_groups_for_mode(&outbound_mode)
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
        let (profile_name, profiles) = profile_catalog.map_or_else(
            |error| {
                tracing::warn!(%error, "failed to read managed profiles for tray menu");
                (
                    profile_path
                        .as_deref()
                        .and_then(|path| path.file_name())
                        .map_or_else(
                            || zenclash_i18n::text("tray.current_profile"),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                    Vec::new(),
                )
            },
            |catalog| {
                let profile_name = catalog.active_profile().map_or_else(
                    || zenclash_i18n::text("tray.current_profile"),
                    |profile| profile.name.clone(),
                );
                let active = catalog.active.as_deref();
                let profiles = catalog
                    .profiles
                    .into_iter()
                    .map(|profile| TrayProfile {
                        active: active == Some(profile.id.as_str()),
                        id: profile.id,
                        name: profile.name,
                    })
                    .collect();
                (profile_name, profiles)
            },
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
            profiles,
            groups,
            directories: tray_directories(profile_path.as_deref(), self.core_kind),
        };
        if let Some(tray) = self.network_tray.as_mut() {
            if let Err(error) = tray.update_menu(&state) {
                tracing::warn!(%error, "failed to update tray menu");
            }
        }
        cx.notify();
    }
}
