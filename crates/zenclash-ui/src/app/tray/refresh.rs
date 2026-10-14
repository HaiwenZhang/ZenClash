use super::runtime_observation::TrayRuntimeObservation;
use super::{
    Context, OutboundMode, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode, ZenClashApp,
};
use crate::runtime_read_source::RuntimeReadSource;
use zenclash_core::{CoreObservationIdentity, Observation, ProxyGroupBehavior};

use super::directories::load_profile_catalog_and_directories;

struct TrayMenuSnapshot {
    config: zenclash_core::RuntimeConfig,
    groups: Vec<TrayProxyGroup>,
    system_proxy: Result<bool, String>,
    profiles: Result<zenclash_core::ProfileCatalog, String>,
    mode_intent_generation: u64,
    observation: CoreObservationIdentity,
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
        let mode_intent_generation = self.outbound_mode.intent_generation();
        let session = self.core_session.clone();
        let operational_status = self.operational_status.clone();
        let read_source = std::sync::Arc::new(RuntimeReadSource::default());
        let submitted_source = read_source.clone();
        let task = self.runtime.spawn(async move {
            let runtime =
                TrayRuntimeObservation::load_with_source(&session, &submitted_source).await?;
            let directories_profile = runtime.profile_path.clone();
            let descriptor = runtime.descriptor;
            let profiles = tokio::task::spawn_blocking(move || {
                load_profile_catalog_and_directories(directories_profile.as_deref(), &descriptor)
            })
            .await;
            let groups = tray_proxy_groups(runtime.catalog, &runtime.config.mode);
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
            Ok::<_, String>(TrayMenuSnapshot {
                config: runtime.config,
                groups,
                system_proxy,
                profiles,
                directories,
                mode_intent_generation,
                observation: runtime.identity,
                profile_path: runtime.profile_path,
            })
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_refreshing = false;
                match result {
                    Ok(Ok(snapshot)) => this.apply_tray_menu_state(snapshot, cx),
                    Ok(Err(error)) => {
                        this.publish_tray_refresh_error(&read_source, error);
                    }
                    Err(error) => {
                        this.publish_tray_refresh_error(&read_source, error.to_string());
                    }
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

    fn publish_tray_refresh_error(&mut self, source: &RuntimeReadSource, error: String) -> bool {
        if self.tray_refresh_pending {
            return false;
        }
        match source.identity() {
            Some(identity) => {
                let published = identity.belongs_to(&self.core_session)
                    && identity
                        .publish_if_current(|| self.tray_error = Some(error))
                        .is_some();
                if !published {
                    self.tray_refresh_pending = true;
                }
                published
            }
            None => {
                // Only preflight/capture can fail here; no controller request has begun.
                self.tray_error = Some(error);
                true
            }
        }
    }

    fn apply_tray_menu_state(&mut self, snapshot: TrayMenuSnapshot, cx: &mut Context<Self>) {
        let TrayMenuSnapshot {
            config,
            groups,
            system_proxy,
            profiles: profile_catalog,
            mode_intent_generation,
            observation,
            directories,
            profile_path,
        } = snapshot;
        if !observation.belongs_to(&self.core_session) {
            self.tray_refresh_pending = true;
            return;
        }
        let mixed_port = config.system_proxy_port().unwrap_or_default();
        let active_profile = profile_catalog
            .as_ref()
            .ok()
            .and_then(|catalog| catalog.active_profile())
            .cloned();
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
        let system_proxy = system_proxy.unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to read system proxy state for tray menu");
            false
        });
        let mut state = TrayMenuState {
            mode: config.mode.clone(),
            system_proxy,
            tun: config.tun.enable,
            mixed_port,
            profile_name,
            profiles,
            groups,
            directories,
        };
        let published = observation.publish_if_current(|| {
            self.outbound_mode
                .synchronize(OutboundMode::from_api(&config.mode), mode_intent_generation);
            state.mode = self.outbound_mode.displayed().api_value().into();
            self.proxies_page.update(cx, |page, cx| {
                page.set_active_profile(active_profile.as_ref(), cx)
            });
            self.tray_state = state;
            self.tray_observation = Some(observation.clone());
            self.tray_error = None;
        });
        if published.is_none() {
            self.tray_refresh_pending = true;
            return;
        }
        // Native menu work stays outside the ordinary-read publication guard.
        if let Some(tray) = self.network_tray.as_mut()
            && let Err(error) =
                tray.update_menu_with_observation(&self.tray_state, Some(observation.clone()))
        {
            tracing::warn!(%error, "failed to update tray menu");
        }
        if !observation.is_current() {
            self.tray_refresh_pending = true;
        }
    }
}

fn tray_proxy_groups(
    catalog: zenclash_core::ProxyCatalog,
    outbound_mode: &str,
) -> Vec<TrayProxyGroup> {
    catalog
        .groups_for_mode(outbound_mode)
        .filter(|group| !group.hidden)
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
                controller_name: group.controller_name().to_owned(),
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
mod observation_tests;

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
