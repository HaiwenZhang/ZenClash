use super::{
    AutostartStatus, ConnectionsSnapshot, Observation, Page, ProviderCatalog, RuleCatalog,
    RuntimeConfig, SystemNetworkSnapshot, SystemProxyStatus, TunPermissionStatus, VersionInfo,
};
use std::path::{Path, PathBuf};
use zenclash_core::ProxyCatalog;

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
                value: ProxyCatalog {
                    proxy_count: 42,
                    ..ProxyCatalog::default()
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
