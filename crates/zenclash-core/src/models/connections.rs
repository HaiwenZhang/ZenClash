use serde::{Deserialize, Serialize};

/// Current connections and aggregate counters from `/connections`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConnectionsSnapshot {
    /// Active connections. Mihomo may encode an empty collection as `null`.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub connections: Vec<Connection>,
    /// Total downloaded bytes since core start.
    #[serde(default, rename = "downloadTotal")]
    pub download_total: u64,
    /// Total uploaded bytes since core start.
    #[serde(default, rename = "uploadTotal")]
    pub upload_total: u64,
    /// Memory usage reported alongside the connection snapshot.
    #[serde(default)]
    pub memory: u64,
}

/// One active Mihomo connection.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Connection {
    /// Stable connection identifier used by the close API.
    #[serde(default)]
    pub id: String,
    /// Network and process metadata for the connection.
    #[serde(default)]
    pub metadata: ConnectionMetadata,
    /// Bytes uploaded by this connection.
    #[serde(default)]
    pub upload: u64,
    /// Bytes downloaded by this connection.
    #[serde(default)]
    pub download: u64,
    /// Mihomo-provided connection start timestamp.
    #[serde(default)]
    pub start: String,
    /// Proxy chain selected for this connection.
    #[serde(default)]
    pub chains: Vec<String>,
    /// Rule type that matched the connection.
    #[serde(default)]
    pub rule: String,
    /// Payload of the matching rule.
    #[serde(default, rename = "rulePayload")]
    pub rule_payload: String,
}

/// Endpoint and owning-process metadata for a connection.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConnectionMetadata {
    /// Transport protocol, typically TCP or UDP.
    #[serde(default)]
    pub network: String,
    /// Mihomo inbound connection type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Source IP address.
    #[serde(default, rename = "sourceIP")]
    pub source_ip: String,
    /// Destination IP address.
    #[serde(default, rename = "destinationIP")]
    pub destination_ip: String,
    /// Source port as reported by Mihomo.
    #[serde(default, rename = "sourcePort")]
    pub source_port: String,
    /// Destination port as reported by Mihomo.
    #[serde(default, rename = "destinationPort")]
    pub destination_port: String,
    /// Sniffed destination hostname.
    #[serde(default)]
    pub host: String,
    /// DNS resolution mode associated with the connection.
    #[serde(default)]
    pub dns_mode: String,
    /// Executable path owning the connection, when available.
    #[serde(default, rename = "processPath")]
    pub process_path: String,
    /// Process name owning the connection, when available.
    #[serde(default)]
    pub process: String,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
