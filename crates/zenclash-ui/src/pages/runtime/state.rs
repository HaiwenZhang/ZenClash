use super::{
    ConnectionsSnapshot, Page, ProviderCatalog, RuleCatalog, RuntimeConfig, SubStoreSnapshot,
    SystemNetworkSnapshot, SystemProxyStatus, VersionInfo,
};

#[derive(Clone, Debug)]
pub(super) enum RuntimeData {
    Empty,
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
    SubStore(SubStoreSnapshot),
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
}
