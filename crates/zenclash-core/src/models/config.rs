use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime settings returned by Mihomo's `/configs` endpoint.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// HTTP proxy port, or zero when disabled.
    #[serde(default)]
    pub port: u16,
    /// SOCKS proxy port, or zero when disabled.
    #[serde(default, rename = "socks-port")]
    pub socks_port: u16,
    /// Combined HTTP/SOCKS proxy port, or zero when disabled.
    #[serde(default, rename = "mixed-port")]
    pub mixed_port: u16,
    /// Linux transparent redirect port.
    #[serde(default, rename = "redir-port")]
    pub redir_port: u16,
    /// Linux TPROXY port.
    #[serde(default, rename = "tproxy-port")]
    pub tproxy_port: u16,
    /// Whether proxy listeners accept LAN clients.
    #[serde(default, rename = "allow-lan")]
    pub allow_lan: bool,
    /// Address used by proxy listeners.
    #[serde(default, rename = "bind-address")]
    pub bind_address: String,
    /// Active outbound mode such as `rule`, `global`, or `direct`.
    #[serde(default)]
    pub mode: String,
    /// Active Mihomo logging level.
    #[serde(default, rename = "log-level")]
    pub log_level: String,
    /// Whether IPv6 handling is enabled.
    #[serde(default)]
    pub ipv6: bool,
    /// Whether TCP connection attempts may run concurrently.
    #[serde(default, rename = "tcp-concurrent")]
    pub tcp_concurrent: bool,
    /// Whether delay measurements use unified-delay semantics.
    #[serde(default, rename = "unified-delay")]
    pub unified_delay: bool,
    /// Explicit outbound network interface.
    #[serde(default, rename = "interface-name")]
    pub interface_name: String,
    /// Runtime TUN settings.
    #[serde(default)]
    pub tun: TunConfig,
    /// Runtime traffic-sniffing settings.
    #[serde(default, alias = "sniffer", deserialize_with = "deserialize_sniffer")]
    pub sniffing: SnifferConfig,
    /// Mihomo fields not yet modeled by `ZenClash`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Runtime subset of Mihomo TUN configuration.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TunConfig {
    /// Whether the TUN stack is enabled.
    #[serde(default)]
    pub enable: bool,
    /// Platform TUN device name.
    #[serde(default)]
    pub device: String,
    /// Network stack implementation selected by Mihomo.
    #[serde(default)]
    pub stack: String,
    /// DNS destinations intercepted by TUN.
    #[serde(default, rename = "dns-hijack")]
    pub dns_hijack: Vec<String>,
    /// Whether Mihomo installs routes automatically.
    #[serde(default, rename = "auto-route")]
    pub auto_route: bool,
    /// Whether Mihomo detects the outbound interface automatically.
    #[serde(default, rename = "auto-detect-interface")]
    pub auto_detect_interface: bool,
    /// Whether strict route handling is enabled.
    #[serde(default, rename = "strict-route")]
    pub strict_route: bool,
    /// Mihomo TUN fields not yet modeled by `ZenClash`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Runtime subset of Mihomo traffic-sniffing configuration.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SnifferConfig {
    /// Whether traffic sniffing is enabled.
    #[serde(default)]
    pub enable: bool,
    /// Whether DNS mapping may be forced for sniffed traffic.
    #[serde(default, rename = "force-dns-mapping")]
    pub force_dns_mapping: bool,
    /// Whether pure-IP traffic may be parsed.
    #[serde(default, rename = "parse-pure-ip")]
    pub parse_pure_ip: bool,
    /// Whether sniffed destinations replace the original target.
    #[serde(default, rename = "override-destination")]
    pub override_destination: bool,
    /// Mihomo sniffer fields not yet modeled by `ZenClash`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn deserialize_sniffer<'de, D>(deserializer: D) -> Result<SnifferConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SnifferValue {
        Enabled(bool),
        Config(SnifferConfig),
    }

    Ok(match SnifferValue::deserialize(deserializer)? {
        SnifferValue::Enabled(enable) => SnifferConfig {
            enable,
            ..Default::default()
        },
        SnifferValue::Config(config) => config,
    })
}
