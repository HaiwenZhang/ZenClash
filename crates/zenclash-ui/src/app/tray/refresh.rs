use super::{
    AppContext, Context, OutboundMode, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode,
    ZenClashApp, tray_directories,
};
use zenclash_core::{Observation, ProxyGroupBehavior, ProxyOperations, ProxyVisibility};

struct TrayMenuSnapshot {
    config: zenclash_core::RuntimeConfig,
    groups: Vec<TrayProxyGroup>,
    system_proxy: Result<bool, String>,
    profiles: Result<zenclash_core::ProfileCatalog, String>,
    mode_generation: u64,
    core_generation: u64,
    directories: Vec<(String, std::path::PathBuf)>,
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
        let core_generation = self.core_session.generation();
        let core_kind = self.core_kind;
        let profile_path = self.profile_path.clone();
        let directories_profile = profile_path.clone();
        let operational_status = self.operational_status.clone();
        let task = self.runtime.spawn(async move {
            let proxy_operations = ProxyOperations::new(client.clone());
            let profile_catalog_task = tokio::task::spawn_blocking(move || {
                let store = zenclash_core::ProfileStore::discover();
                let data_root = store
                    .as_ref()
                    .ok()
                    .map(|store| store.root().parent().unwrap_or(store.root()));
                let directories =
                    tray_directories(directories_profile.as_deref(), core_kind, data_root);
                let catalog = store
                    .and_then(|store| store.load())
                    .map_err(|error| error.to_string());
                (catalog, directories)
            });
            let (config, catalog, profiles) = tokio::join!(
                client.runtime_config(),
                proxy_operations.catalog(ProxyVisibility::VisibleOnly),
                profile_catalog_task
            );
            let config = config.map_err(|error| error.to_string())?;
            let catalog = catalog.map_err(|error| error.to_string())?;
            let groups = tray_proxy_groups(catalog, &config.mode);
            let system_proxy = match operational_status.snapshot().capture.system_proxy {
                Observation::Fresh { value, .. } | Observation::Stale { value, .. } => {
                    Ok(value.actual.active())
                }
                Observation::Failed { failure, .. } => Err(failure.message),
                Observation::Loading => Err(zenclash_i18n::text("system_proxy.status.loading")),
            };
            let (profiles, directories) = profiles.unwrap_or_else(|error| {
                (
                    Err(zenclash_i18n::text_with(
                        "tray.errors.config_directory",
                        &[("error", error.to_string())],
                    )),
                    Vec::new(),
                )
            });
            Ok::<_, String>((
                config,
                groups,
                system_proxy,
                profiles,
                directories,
                mode_generation,
            ))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_refreshing = false;
                match result {
                    Ok(Ok((
                        config,
                        groups,
                        system_proxy,
                        profiles,
                        directories,
                        mode_generation,
                    ))) => {
                        this.apply_tray_menu_state(
                            TrayMenuSnapshot {
                                config,
                                groups,
                                system_proxy,
                                profiles,
                                mode_generation,
                                core_generation,
                                directories,
                                profile_path,
                            },
                            cx,
                        );
                    }
                    Ok(Err(error)) => this.tray_error = Some(error),
                    Err(error) => this.tray_error = Some(error.to_string()),
                }
                cx.notify();

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
            groups,
            system_proxy,
            profiles: profile_catalog,
            mode_generation,
            core_generation,
            directories,
            profile_path,
        } = snapshot;
        if self.core_session.generation() != core_generation {
            self.tray_refresh_pending = true;
            return;
        }
        self.outbound_mode
            .synchronize(OutboundMode::from_api(&config.mode), mode_generation);
        let mixed_port = config.system_proxy_port().unwrap_or_default();
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
            directories,
        };
        if let Some(tray) = self.network_tray.as_mut()
            && let Err(error) = tray.update_menu(&state)
        {
            tracing::warn!(%error, "failed to update tray menu");
        }
        self.tray_state = state;
        self.tray_error = None;
    }
}

fn tray_proxy_groups(
    catalog: zenclash_core::ProxyCatalog,
    outbound_mode: &str,
) -> Vec<TrayProxyGroup> {
    catalog
        .groups_for_mode(outbound_mode)
        .map(|group| {
            let selectable = matches!(
                &group.behavior,
                ProxyGroupBehavior::Selector | ProxyGroupBehavior::Automatic { .. }
            );
            let automatic = matches!(&group.behavior, ProxyGroupBehavior::Automatic { .. });
            let (proxies, has_more) = bounded_tray_proxy_nodes(&catalog, &group.all, &group.now);
            TrayProxyGroup {
                selectable,
                automatic,
                has_more,
                name: group.name.clone(),
                now: group.now.clone(),
                test_url: group.test_url.clone(),
                proxies: proxies.into(),
            }
        })
        .collect()
}

fn bounded_tray_proxy_nodes(
    catalog: &zenclash_core::ProxyCatalog,
    proxies: &[zenclash_core::ProxyNodeId],
    current: &str,
) -> (Vec<TrayProxyNode>, bool) {
    let limit = crate::components::tray::MAX_TRAY_PROXY_NODES;
    let has_more = proxies.len() > limit;
    let current_index = has_more
        .then(|| {
            proxies
                .iter()
                .position(|id| id.controller_name() == current)
        })
        .flatten();
    let proxies = (0..proxies.len().min(limit))
        .filter_map(|index| {
            let index = if index + 1 == limit {
                current_index
                    .filter(|&index| index >= limit)
                    .unwrap_or(index)
            } else {
                index
            };
            let id = &proxies[index];
            let proxy = catalog.node(id)?;
            Some(TrayProxyNode {
                name: id.controller_name().to_owned(),
                provider: proxy.provider_name.clone(),
                delay: proxy.latest_delay(),
            })
        })
        .collect();
    (proxies, has_more)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_tray_nodes_keep_the_current_member_without_retaining_the_full_group() {
        let proxies = (0..30)
            .map(|index| zenclash_core::ProxyNode {
                name: format!("node-{index}"),
                ..zenclash_core::ProxyNode::default()
            })
            .collect();

        let catalog = zenclash_core::ProxyCatalog::from_group_nodes(
            vec![(zenclash_core::ProxyGroup::default(), proxies)],
            30,
        );
        let (nodes, has_more) =
            bounded_tray_proxy_nodes(&catalog, &catalog.groups()[0].all, "node-29");

        assert_eq!(
            (
                nodes.len(),
                nodes.iter().any(|node| node.name == "node-29"),
                has_more,
            ),
            (crate::components::tray::MAX_TRAY_PROXY_NODES, true, true)
        );
    }
}
