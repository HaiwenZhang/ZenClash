use gpui_component::IconName;

/// Live proxy-group selection page.
pub mod proxies;
/// Shared host for Mihomo configuration, status, and diagnostics pages.
pub mod runtime;

/// Stable identity of every destination in the `ZenClash` navigation model.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Page {
    /// Operating-system HTTP and HTTPS proxy control.
    SystemProxy,
    /// Mihomo TUN virtual-interface configuration.
    Tun,
    /// Local profiles and online subscription management.
    Profiles,
    /// Proxy groups, nodes, selection, and delay testing.
    #[default]
    Proxies,
    /// Managed Mihomo process and controller information.
    Mihomo,
    /// Active network connections.
    Connections,
    /// Mihomo DNS configuration.
    Dns,
    /// Protocol and hostname sniffing configuration.
    Sniffer,
    /// Live Mihomo log stream.
    Logs,
    /// Active routing rules.
    Rules,
    /// Proxy and rule providers.
    Resources,
    /// Ordered YAML override chain.
    Override,
    /// External Sub-Store service catalog.
    SubStore,
    /// Operating-system network information.
    Network,
    /// Live and historical traffic presentation.
    Traffic,
    /// `ZenClash` application preferences.
    Settings,
}

impl Page {
    /// Pages grouped under the sidebar overview section.
    pub const OVERVIEW: [Self; 4] = [
        Self::Proxies,
        Self::Connections,
        Self::Traffic,
        Self::Network,
    ];

    /// Pages grouped under the sidebar routing section.
    pub const ROUTING: [Self; 4] = [Self::SystemProxy, Self::Tun, Self::Dns, Self::Sniffer];

    /// Pages grouped under the sidebar configuration section.
    pub const CONFIGURATION: [Self; 5] = [
        Self::Profiles,
        Self::Rules,
        Self::Resources,
        Self::Override,
        Self::SubStore,
    ];

    /// Pages grouped under the sidebar system section.
    pub const SYSTEM: [Self; 2] = [Self::Mihomo, Self::Logs];

    /// Ordered pages rendered as primary sidebar cards.
    pub const SIDEBAR_CARDS: [Self; 15] = [
        Self::Proxies,
        Self::Connections,
        Self::Traffic,
        Self::Network,
        Self::SystemProxy,
        Self::Tun,
        Self::Dns,
        Self::Sniffer,
        Self::Profiles,
        Self::Rules,
        Self::Resources,
        Self::Override,
        Self::SubStore,
        Self::Mihomo,
        Self::Logs,
    ];

    /// Returns the localized navigation label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SystemProxy => "系统代理",
            Self::Tun => "虚拟网卡",
            Self::Profiles => "订阅管理",
            Self::Proxies => "代理组",
            Self::Mihomo => "内核设置",
            Self::Connections => "连接",
            Self::Dns => "DNS 覆写",
            Self::Sniffer => "嗅探覆写",
            Self::Logs => "日志",
            Self::Rules => "规则",
            Self::Resources => "外部资源",
            Self::Override => "覆写",
            Self::SubStore => "Sub-Store",
            Self::Network => "网络信息",
            Self::Traffic => "用量",
            Self::Settings => "应用设置",
        }
    }

    /// Returns the uppercase sidebar section label.
    #[must_use]
    pub const fn section_label(self) -> &'static str {
        match self {
            Self::Proxies | Self::Connections | Self::Traffic | Self::Network => "OVERVIEW",
            Self::SystemProxy | Self::Tun | Self::Dns | Self::Sniffer => "ROUTING",
            Self::Profiles | Self::Rules | Self::Resources | Self::Override | Self::SubStore => {
                "CONFIGURATION"
            }
            Self::Mihomo | Self::Logs => "SYSTEM",
            Self::Settings => "PREFERENCES",
        }
    }

    /// Returns the stable element identifier used by navigation controls.
    #[must_use]
    pub const fn route(self) -> &'static str {
        match self {
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
            Self::SubStore => "substore",
            Self::Network => "network",
            Self::Traffic => "traffic",
            Self::Settings => "settings",
        }
    }

    /// Returns the gpui-component icon associated with this destination.
    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
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
            Self::SubStore => IconName::Folder,
            Self::Network => IconName::Inspector,
            Self::Traffic => IconName::ChartPie,
            Self::Settings => IconName::Settings2,
        }
    }

    /// Returns the localized one-line description shown in page headers.
    #[must_use]
    pub const fn subtitle(self) -> &'static str {
        match self {
            Self::SystemProxy => "配置操作系统 HTTP/HTTPS 代理与绕过地址。",
            Self::Tun => "管理 Mihomo TUN 虚拟网卡、路由与权限。",
            Self::Profiles => "添加、更新、编辑并切换本地或远程订阅。",
            Self::Proxies => "查看代理组、切换节点并执行延迟测试。",
            Self::Mihomo => "管理内核版本、启动参数与控制器设置。",
            Self::Connections => "查看实时连接、速率、规则链并关闭连接。",
            Self::Dns => "配置 DNS 监听、上游服务器、Fake-IP 与回退策略。",
            Self::Sniffer => "配置 TLS、HTTP、QUIC 域名嗅探规则。",
            Self::Logs => "筛选并跟踪 Mihomo 实时日志。",
            Self::Rules => "浏览当前运行时规则和命中策略。",
            Self::Resources => "更新代理提供者、规则提供者与 GeoData。",
            Self::Override => "按顺序管理脚本、YAML 与远程覆写。",
            Self::SubStore => "管理 Sub-Store 服务与订阅处理页面。",
            Self::Network => "查看网络接口、出口地址和连通性。",
            Self::Traffic => "按主机、来源、出站和进程分析历史用量。",
            Self::Settings => "配置语言、主题、侧栏、托盘、快捷键与备份。",
        }
    }
}
