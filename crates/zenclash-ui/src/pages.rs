use gpui_component::IconName;

pub mod proxies;
pub mod runtime;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Page {
    SystemProxy,
    Tun,
    Profiles,
    #[default]
    Proxies,
    Mihomo,
    Connections,
    Dns,
    Sniffer,
    Logs,
    Rules,
    Resources,
    Override,
    SubStore,
    Network,
    Traffic,
    Settings,
}

impl Page {
    pub const SIDEBAR_CARDS: [Page; 15] = [
        Page::SystemProxy,
        Page::Tun,
        Page::Profiles,
        Page::Proxies,
        Page::Mihomo,
        Page::Connections,
        Page::Dns,
        Page::Sniffer,
        Page::Logs,
        Page::Rules,
        Page::Resources,
        Page::Override,
        Page::SubStore,
        Page::Network,
        Page::Traffic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Page::SystemProxy => "系统代理",
            Page::Tun => "虚拟网卡",
            Page::Profiles => "订阅管理",
            Page::Proxies => "代理组",
            Page::Mihomo => "内核设置",
            Page::Connections => "连接",
            Page::Dns => "DNS 覆写",
            Page::Sniffer => "嗅探覆写",
            Page::Logs => "日志",
            Page::Rules => "规则",
            Page::Resources => "外部资源",
            Page::Override => "覆写",
            Page::SubStore => "Sub-Store",
            Page::Network => "网络信息",
            Page::Traffic => "用量",
            Page::Settings => "应用设置",
        }
    }

    pub fn route(self) -> &'static str {
        match self {
            Page::SystemProxy => "sysproxy",
            Page::Tun => "tun",
            Page::Profiles => "profiles",
            Page::Proxies => "proxies",
            Page::Mihomo => "mihomo",
            Page::Connections => "connections",
            Page::Dns => "dns",
            Page::Sniffer => "sniffer",
            Page::Logs => "logs",
            Page::Rules => "rules",
            Page::Resources => "resources",
            Page::Override => "override",
            Page::SubStore => "substore",
            Page::Network => "network",
            Page::Traffic => "traffic",
            Page::Settings => "settings",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Page::SystemProxy | Page::Proxies | Page::Network => IconName::Globe,
            Page::Tun => IconName::Map,
            Page::Profiles | Page::Override => IconName::File,
            Page::Mihomo | Page::Settings => IconName::Settings,
            Page::Connections => IconName::ExternalLink,
            Page::Dns => IconName::Building2,
            Page::Sniffer => IconName::Search,
            Page::Logs => IconName::BookOpen,
            Page::Rules => IconName::Menu,
            Page::Resources => IconName::Inbox,
            Page::SubStore | Page::Traffic => IconName::Folder,
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Page::SystemProxy => "配置操作系统 HTTP/HTTPS 代理与绕过地址。",
            Page::Tun => "管理 Mihomo TUN 虚拟网卡、路由与权限。",
            Page::Profiles => "添加、更新、编辑并切换本地或远程订阅。",
            Page::Proxies => "查看代理组、切换节点并执行延迟测试。",
            Page::Mihomo => "管理内核版本、启动参数与控制器设置。",
            Page::Connections => "查看实时连接、速率、规则链并关闭连接。",
            Page::Dns => "配置 DNS 监听、上游服务器、Fake-IP 与回退策略。",
            Page::Sniffer => "配置 TLS、HTTP、QUIC 域名嗅探规则。",
            Page::Logs => "筛选并跟踪 Mihomo 实时日志。",
            Page::Rules => "浏览当前运行时规则和命中策略。",
            Page::Resources => "更新代理提供者、规则提供者与 GeoData。",
            Page::Override => "按顺序管理脚本、YAML 与远程覆写。",
            Page::SubStore => "管理 Sub-Store 服务与订阅处理页面。",
            Page::Network => "查看网络接口、出口地址和连通性。",
            Page::Traffic => "按主机、来源、出站和进程分析历史用量。",
            Page::Settings => "配置语言、主题、侧栏、托盘、快捷键与备份。",
        }
    }
}
