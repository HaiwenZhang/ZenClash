use gpui_component::IconName;

/// Live proxy-group selection page.
pub mod proxies;
/// Shared host for runtime-core configuration, status, and diagnostics pages.
pub mod runtime;

/// Stable identity of every destination in the `ZenClash` navigation model.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Page {
    /// Compact operational summary and primary controls.
    #[default]
    Home,
    /// Operating-system HTTP and HTTPS proxy control.
    SystemProxy,
    /// Runtime-core TUN virtual-interface configuration.
    Tun,
    /// Local profiles and online subscription management.
    Profiles,
    /// Proxy groups, nodes, selection, and delay testing.
    Proxies,
    /// Managed runtime process and controller information.
    Mihomo,
    /// Active network connections.
    Connections,
    /// Runtime-core DNS configuration.
    Dns,
    /// Protocol and hostname sniffing configuration.
    Sniffer,
    /// Live runtime-core log stream.
    Logs,
    /// Active routing rules.
    Rules,
    /// Proxy and rule providers.
    Resources,
    /// Ordered YAML override chain.
    Override,
    /// Operating-system network information.
    Network,
    /// Live and historical traffic presentation.
    Traffic,
    /// `ZenClash` application preferences.
    Settings,
}

impl Page {
    /// Everyday destinations kept visible in the compact sidebar.
    ///
    /// Home and Settings are rendered separately.
    pub const PRIMARY: [Self; 7] = [
        Self::Proxies,
        Self::Profiles,
        Self::Connections,
        Self::Rules,
        Self::Network,
        Self::Traffic,
        Self::Logs,
    ];

    /// Returns the localized navigation label.
    #[must_use]
    pub fn label(self) -> String {
        zenclash_i18n::text(match self {
            Self::Home => "navigation.home.label",
            Self::SystemProxy => "navigation.system_proxy.label",
            Self::Tun => "navigation.tun.label",
            Self::Profiles => "navigation.profiles.label",
            Self::Proxies => "navigation.proxies.label",
            Self::Mihomo => "navigation.mihomo.label",
            Self::Connections => "navigation.connections.label",
            Self::Dns => "navigation.dns.label",
            Self::Sniffer => "navigation.sniffer.label",
            Self::Logs => "navigation.logs.label",
            Self::Rules => "navigation.rules.label",
            Self::Resources => "navigation.resources.label",
            Self::Override => "navigation.override.label",
            Self::Network => "navigation.network.label",
            Self::Traffic => "navigation.traffic.label",
            Self::Settings => "navigation.settings.label",
        })
    }

    /// Returns the stable element identifier used by navigation controls.
    #[must_use]
    pub const fn route(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::SystemProxy => "sysproxy",
            Self::Tun => "tun",
            Self::Profiles => "profiles",
            Self::Proxies => "proxies",
            Self::Mihomo => "mihomo",
            Self::Connections => "connections",
            Self::Dns => "dns",
            Self::Sniffer => "sniffer",
            Self::Logs => "logs",
            Self::Rules => "rules",
            Self::Resources => "resources",
            Self::Override => "override",
            Self::Network => "network",
            Self::Traffic => "traffic",
            Self::Settings => "settings",
        }
    }

    /// Returns the gpui-component icon associated with this destination.
    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
            Self::Home => IconName::LayoutDashboard,
            Self::SystemProxy => IconName::Globe,
            Self::Tun => IconName::Map,
            Self::Profiles => IconName::FolderOpen,
            Self::Proxies => IconName::GalleryVerticalEnd,
            Self::Mihomo => IconName::Bot,
            Self::Connections => IconName::ExternalLink,
            Self::Dns => IconName::Building2,
            Self::Sniffer => IconName::Search,
            Self::Logs => IconName::SquareTerminal,
            Self::Rules => IconName::Menu,
            Self::Resources => IconName::Inbox,
            Self::Override => IconName::Replace,
            Self::Network => IconName::Globe,
            Self::Traffic => IconName::ChartPie,
            Self::Settings => IconName::Settings2,
        }
    }

    /// Returns the localized one-line description shown in page headers.
    #[must_use]
    pub fn subtitle(self) -> String {
        zenclash_i18n::text(match self {
            Self::Home => "navigation.home.subtitle",
            Self::SystemProxy => "navigation.system_proxy.subtitle",
            Self::Tun => "navigation.tun.subtitle",
            Self::Profiles => "navigation.profiles.subtitle",
            Self::Proxies => "navigation.proxies.subtitle",
            Self::Mihomo => "navigation.mihomo.subtitle",
            Self::Connections => "navigation.connections.subtitle",
            Self::Dns => "navigation.dns.subtitle",
            Self::Sniffer => "navigation.sniffer.subtitle",
            Self::Logs => "navigation.logs.subtitle",
            Self::Rules => "navigation.rules.subtitle",
            Self::Resources => "navigation.resources.subtitle",
            Self::Override => "navigation.override.subtitle",
            Self::Network => "navigation.network.subtitle",
            Self::Traffic => "navigation.traffic.subtitle",
            Self::Settings => "navigation.settings.subtitle",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn primary_navigation_exposes_frequent_runtime_destinations() {
        let destinations = std::iter::once(Page::Home)
            .chain(Page::PRIMARY)
            .chain(std::iter::once(Page::Settings))
            .collect::<Vec<_>>();

        assert_eq!(
            destinations,
            [
                Page::Home,
                Page::Proxies,
                Page::Profiles,
                Page::Connections,
                Page::Rules,
                Page::Network,
                Page::Traffic,
                Page::Logs,
                Page::Settings,
            ]
        );
    }
}
