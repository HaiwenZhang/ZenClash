use super::{
    AutostartStatus, ConnectionsSnapshot, Observation, Page, ProviderCatalog, RuleCatalog,
    RuntimeConfig, SystemNetworkSnapshot, SystemProxyStatus, TunPermissionStatus, VersionInfo,
};
use zenclash_core::ProxyCatalog;

#[derive(Clone, Debug)]
pub(super) enum RuntimeData {
    Empty,
    Dashboard {
        config: Observation<RuntimeConfig>,
        proxies: Observation<ProxyCatalog>,
        connections: Observation<ConnectionsSnapshot>,
    },
    Config(RuntimeConfig),
    Core {
        version: VersionInfo,
        config: RuntimeConfig,
    },
    Profile {
        config: RuntimeConfig,
        proxy_count: usize,
        group_count: usize,
        rule_count: usize,
    },
    Connections(ConnectionsSnapshot),
    Rules(RuleCatalog),
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
        permissions: Result<TunPermissionStatus, String>,
    },
    Settings {
        config: RuntimeConfig,
        autostart: AutostartStatus,
    },
}

impl RuntimeData {
    pub(super) fn retain_dashboard_successes(self, previous: &Self) -> Self {
        let Self::Dashboard {
            config,
            proxies,
            connections,
        } = self
        else {
            return self;
        };
        let Self::Dashboard {
            config: previous_config,
            proxies: previous_proxies,
            connections: previous_connections,
        } = previous
        else {
            return Self::Dashboard {
                config,
                proxies,
                connections,
            };
        };
        Self::Dashboard {
            config: Observation::retain_last_success(previous_config, config),
            proxies: Observation::retain_last_success(previous_proxies, proxies),
            connections: Observation::retain_last_success(previous_connections, connections),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PageTaskToken {
    pub(super) page: Page,
    pub(super) navigation_generation: u64,
}

impl PageTaskToken {
    pub(super) fn is_current(self, page: Page, navigation_generation: u64) -> bool {
        self.page == page && self.navigation_generation == navigation_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{OperationalFailure, RecoveryAction};

    #[test]
    fn page_task_token_rejects_same_page_after_navigation_round_trip() {
        let token = PageTaskToken {
            page: Page::Profiles,
            navigation_generation: 3,
        };

        assert!(!token.is_current(Page::Profiles, 5));
    }

    #[test]
    fn page_task_token_accepts_unchanged_page_generation() {
        let token = PageTaskToken {
            page: Page::Resources,
            navigation_generation: 8,
        };

        assert!(token.is_current(Page::Resources, 8));
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
                value: ProxyCatalog::default(),
                observed_at_ms: 10,
            },
            connections: Observation::Fresh {
                value: ConnectionsSnapshot {
                    memory: 42,
                    ..ConnectionsSnapshot::default()
                },
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
            proxies: Observation::Fresh {
                value: ProxyCatalog::default(),
                observed_at_ms: 20,
            },
            connections: failure,
        }
        .retain_dashboard_successes(&previous);

        let RuntimeData::Dashboard {
            config,
            connections,
            ..
        } = next
        else {
            panic!("expected dashboard data");
        };
        assert_eq!(
            config.value().map(|config| config.mode.as_str()),
            Some("direct")
        );
        assert!(matches!(
            connections,
            Observation::Stale {
                value: ConnectionsSnapshot { memory: 42, .. },
                observed_at_ms: 10,
                ..
            }
        ));
    }
}
