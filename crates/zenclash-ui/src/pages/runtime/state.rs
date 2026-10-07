use super::{
    AutostartStatus, ConnectionsSnapshot, CoreTunPermissionStatus, Observation, Page,
    ProviderCatalog, RuleCatalog, RuntimeConfig, SystemNetworkSnapshot, SystemProxyStatus,
    VersionInfo,
};
use std::path::{Path, PathBuf};
use zenclash_core::ProxyCatalog;

/// Recently visited pages retain their last snapshot while a new read is pending.
/// Move snapshots rather than cloning potentially large proxy and rule catalogs.
#[derive(Default)]
pub(super) struct PageSnapshots {
    entries: std::collections::VecDeque<(Page, u64, RuntimeData)>,
}

impl PageSnapshots {
    const CAPACITY: usize = 4;

    pub(super) fn store(&mut self, page: Page, version: u64, data: RuntimeData) {
        if matches!(data, RuntimeData::Empty) {
            return;
        }
        self.entries
            .retain(|(key, revision, _)| *key != page && *revision == version);
        if self.entries.len() == Self::CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((page, version, data));
    }

    pub(super) fn take(&mut self, page: Page, version: u64) -> RuntimeData {
        self.entries.retain(|(_, revision, _)| *revision == version);
        self.entries
            .iter()
            .position(|(key, _, _)| *key == page)
            .and_then(|index| self.entries.remove(index))
            .map_or(RuntimeData::Empty, |(_, _, data)| data)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone, Debug)]
pub(super) enum RuntimeData {
    Empty,
    Dashboard {
        config: Observation<RuntimeConfig>,
        proxies: Observation<ProxyCatalog>,
    },
    Config(RuntimeConfig),
    Core {
        version: VersionInfo,
        config: RuntimeConfig,
    },
    Profile {
        config: Option<RuntimeConfig>,
        proxy_count: Option<usize>,
        group_count: Option<usize>,
        rule_count: Option<usize>,
    },
    Connections(std::sync::Arc<ConnectionsSnapshot>),
    Rules {
        catalog: std::sync::Arc<RuleCatalog>,
        config: Option<RuntimeConfig>,
        proxies: Option<ProxyCatalog>,
    },
    Resources {
        config: RuntimeConfig,
        proxy: ProviderCatalog,
        rules: ProviderCatalog,
    },
    SystemProxy {
        config: RuntimeConfig,
        status: SystemProxyStatus,
    },
    Network {
        config: RuntimeConfig,
        system: SystemNetworkSnapshot,
    },
    Tun {
        config: RuntimeConfig,
        permissions: Observation<CoreTunPermissionStatus>,
    },
    Settings {
        config: Option<RuntimeConfig>,
        autostart: Result<AutostartStatus, String>,
    },
}

impl RuntimeData {
    pub(super) fn retain_dashboard_successes(self, previous: &Self) -> Self {
        let Self::Dashboard { config, proxies } = self else {
            return self;
        };
        let Self::Dashboard {
            config: previous_config,
            proxies: previous_proxies,
        } = previous
        else {
            return Self::Dashboard { config, proxies };
        };
        Self::Dashboard {
            config: Observation::retain_last_success(previous_config, config),
            proxies: Observation::retain_last_success(previous_proxies, proxies),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PageTaskToken {
    pub(super) page: Page,
    pub(super) navigation_generation: u64,
    pub(super) mutation: Option<super::busy::MutationToken>,
}

impl PageTaskToken {
    pub(super) fn is_current(self, page: Page, navigation_generation: u64) -> bool {
        self.page == page && self.navigation_generation == navigation_generation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConfigInputsTaskToken {
    pub(super) profile: PathBuf,
    pub(super) generation: u64,
}

impl ConfigInputsTaskToken {
    pub(super) fn is_current(&self, profile: Option<&Path>, generation: u64) -> bool {
        profile == Some(self.profile.as_path()) && self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{OperationalFailure, RecoveryAction};

    #[test]
    fn page_snapshots_never_restore_a_previous_runtime() {
        let mut snapshots = PageSnapshots::default();
        snapshots.store(Page::Dns, 1, RuntimeData::Config(RuntimeConfig::default()));
        assert!(matches!(snapshots.take(Page::Dns, 2), RuntimeData::Empty));
    }

    #[test]
    fn page_snapshots_bound_retained_pages_and_move_large_catalogs() {
        let mut snapshots = PageSnapshots::default();
        let catalog = std::sync::Arc::new(RuleCatalog::default());
        snapshots.store(
            Page::Rules,
            1,
            RuntimeData::Rules {
                catalog: catalog.clone(),
                config: None,
                proxies: None,
            },
        );
        let RuntimeData::Rules {
            catalog: restored, ..
        } = snapshots.take(Page::Rules, 1)
        else {
            panic!("expected retained rules");
        };
        assert!(std::sync::Arc::ptr_eq(&catalog, &restored));
        for page in [
            Page::Dns,
            Page::Sniffer,
            Page::Tun,
            Page::Mihomo,
            Page::Resources,
        ] {
            snapshots.store(page, 1, RuntimeData::Config(RuntimeConfig::default()));
        }
        assert!(matches!(snapshots.take(Page::Dns, 1), RuntimeData::Empty));
        assert!(matches!(
            snapshots.take(Page::Sniffer, 1),
            RuntimeData::Config(_)
        ));
    }

    #[test]
    fn page_task_token_rejects_same_page_after_navigation_round_trip() {
        let token = PageTaskToken {
            page: Page::Profiles,
            navigation_generation: 3,
            mutation: None,
        };

        assert!(!token.is_current(Page::Profiles, 5));
    }

    #[test]
    fn page_task_token_accepts_unchanged_page_generation() {
        let token = PageTaskToken {
            page: Page::Resources,
            navigation_generation: 8,
            mutation: None,
        };

        assert!(token.is_current(Page::Resources, 8));
    }

    #[test]
    fn config_inputs_task_rejects_result_for_replaced_profile() {
        let token = ConfigInputsTaskToken {
            profile: PathBuf::from("profiles/old.yaml"),
            generation: 4,
        };

        assert!(!token.is_current(Some(Path::new("profiles/new.yaml")), 4));
    }

    #[test]
    fn config_inputs_task_rejects_result_after_same_profile_is_invalidated() {
        let token = ConfigInputsTaskToken {
            profile: PathBuf::from("profiles/active.yaml"),
            generation: 4,
        };

        assert!(!token.is_current(Some(Path::new("profiles/active.yaml")), 5));
        assert!(token.is_current(Some(Path::new("profiles/active.yaml")), 4));
    }

    #[test]
    fn dashboard_failure_keeps_only_the_affected_last_successful_slice() {
        let previous = RuntimeData::Dashboard {
            config: Observation::Fresh {
                value: RuntimeConfig {
                    mode: "rule".into(),
                    ..RuntimeConfig::default()
                },
                observed_at_ms: 10,
            },
            proxies: Observation::Fresh {
                value: ProxyCatalog::from_group_nodes(Vec::new(), 42),
                observed_at_ms: 10,
            },
        };
        let failure = Observation::Failed {
            failure: OperationalFailure {
                message: "offline".into(),
                occurred_at_ms: 20,
            },
            recovery: RecoveryAction::Retry,
        };
        let next = RuntimeData::Dashboard {
            config: Observation::Fresh {
                value: RuntimeConfig {
                    mode: "direct".into(),
                    ..RuntimeConfig::default()
                },
                observed_at_ms: 20,
            },
            proxies: failure,
        }
        .retain_dashboard_successes(&previous);

        let RuntimeData::Dashboard {
            config, proxies, ..
        } = next
        else {
            panic!("expected dashboard data");
        };
        assert_eq!(
            config.value().map(|config| config.mode.as_str()),
            Some("direct")
        );
        assert!(matches!(
            proxies,
            Observation::Stale {
                value: ProxyCatalog {
                    proxy_count: 42,
                    ..
                },
                observed_at_ms: 10,
                ..
            }
        ));
    }
}
