//! Payloads accepted by the ordinary page observation owner.
//! Original observation identities remain with that owner, outside these values.

use zenclash_core::{
    AutostartStatus, ConnectionsSnapshot, CoreTunPermissionStatus, Observation, ProviderCatalog,
    ProxyCatalog, RuleCatalog, RuntimeConfig, SystemNetworkSnapshot, SystemProxySummary,
    VersionInfo,
};

#[derive(Clone, Debug)]
pub(in crate::pages::runtime) enum PageSnapshotData {
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
        status: SystemProxySummary,
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

impl PageSnapshotData {
    pub(in crate::pages::runtime) fn retain_dashboard_successes(self, previous: &Self) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{OperationalFailure, RecoveryAction};
    #[test]
    fn dashboard_failure_keeps_only_the_affected_last_successful_slice() {
        let previous = PageSnapshotData::Dashboard {
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
        let next = PageSnapshotData::Dashboard {
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

        let PageSnapshotData::Dashboard {
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
