use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub port: u16,
    #[serde(default, rename = "socks-port")]
    pub socks_port: u16,
    #[serde(default, rename = "mixed-port")]
    pub mixed_port: u16,
    #[serde(default, rename = "redir-port")]
    pub redir_port: u16,
    #[serde(default, rename = "tproxy-port")]
    pub tproxy_port: u16,
    #[serde(default, rename = "allow-lan")]
    pub allow_lan: bool,
    #[serde(default, rename = "bind-address")]
    pub bind_address: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default, rename = "log-level")]
    pub log_level: String,
    #[serde(default)]
    pub ipv6: bool,
    #[serde(default, rename = "tcp-concurrent")]
    pub tcp_concurrent: bool,
    #[serde(default, rename = "unified-delay")]
    pub unified_delay: bool,
    #[serde(default, rename = "interface-name")]
    pub interface_name: String,
    #[serde(default)]
    pub tun: TunConfig,
    #[serde(default, alias = "sniffer", deserialize_with = "deserialize_sniffer")]
    pub sniffing: SnifferConfig,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TunConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub stack: String,
    #[serde(default, rename = "dns-hijack")]
    pub dns_hijack: Vec<String>,
    #[serde(default, rename = "auto-route")]
    pub auto_route: bool,
    #[serde(default, rename = "auto-detect-interface")]
    pub auto_detect_interface: bool,
    #[serde(default, rename = "strict-route")]
    pub strict_route: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SnifferConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default, rename = "force-dns-mapping")]
    pub force_dns_mapping: bool,
    #[serde(default, rename = "parse-pure-ip")]
    pub parse_pure_ip: bool,
    #[serde(default, rename = "override-destination")]
    pub override_destination: bool,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuleCatalog {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Rule {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub proxy: String,
    #[serde(default)]
    pub size: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ConnectionsSnapshot {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub connections: Vec<Connection>,
    #[serde(default, rename = "downloadTotal")]
    pub download_total: u64,
    #[serde(default, rename = "uploadTotal")]
    pub upload_total: u64,
    #[serde(default)]
    pub memory: u64,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Connection {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub metadata: ConnectionMetadata,
    #[serde(default)]
    pub upload: u64,
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub chains: Vec<String>,
    #[serde(default)]
    pub rule: String,
    #[serde(default, rename = "rulePayload")]
    pub rule_payload: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ConnectionMetadata {
    #[serde(default)]
    pub network: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, rename = "sourceIP")]
    pub source_ip: String,
    #[serde(default, rename = "destinationIP")]
    pub destination_ip: String,
    #[serde(default, rename = "sourcePort")]
    pub source_port: String,
    #[serde(default, rename = "destinationPort")]
    pub destination_port: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub dns_mode: String,
    #[serde(default, rename = "processPath")]
    pub process_path: String,
    #[serde(default)]
    pub process: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ProviderCatalog {
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Provider {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, rename = "vehicleType")]
    pub vehicle_type: String,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
    #[serde(default, rename = "testUrl")]
    pub test_url: String,
    #[serde(default)]
    pub proxies: Vec<Value>,
    #[serde(default)]
    pub rule_count: usize,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemorySnapshot {
    #[serde(default)]
    pub inuse: u64,
    #[serde(default)]
    pub oslimit: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_real_mihomo_runtime_shapes() {
        let config: RuntimeConfig = serde_json::from_str(
            r#"{"port":7890,"socks-port":7891,"mode":"rule","log-level":"info","ipv6":true,"tun":{"enable":false,"stack":"mixed","auto-route":true},"sniffing":{"enable":true}}"#,
        )
        .unwrap();
        assert_eq!(config.socks_port, 7891);
        assert!(config.ipv6);
        assert_eq!(config.tun.stack, "mixed");
        assert!(config.sniffing.enable);

        let rules: RuleCatalog = serde_json::from_str(
            r#"{"rules":[{"type":"Domain","payload":"example.com","proxy":"DIRECT","size":-1}]}"#,
        )
        .unwrap();
        assert_eq!(rules.rules[0].kind, "Domain");
        assert_eq!(rules.rules[0].size, -1);
    }
}
