use std::fmt;

use serde::{
    Deserialize, Serialize,
    de::{IgnoredAny, SeqAccess, Visitor},
};

/// Aggregate fields from `/connections` without retaining per-connection metadata.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ConnectionsSummary {
    /// Number of active connections in the snapshot.
    #[serde(
        default,
        rename = "connections",
        deserialize_with = "deserialize_connection_count"
    )]
    pub active_connections: usize,
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

/// Connection fields required by background traffic accounting.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TrafficAccountingSnapshot {
    /// Active connections with only accounting-relevant metadata retained.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub connections: Vec<TrafficAccountingConnection>,
    /// Total downloaded bytes since core start.
    #[serde(default, rename = "downloadTotal")]
    pub download_total: u64,
    /// Total uploaded bytes since core start.
    #[serde(default, rename = "uploadTotal")]
    pub upload_total: u64,
}

/// Minimal per-connection state used to calculate traffic deltas.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TrafficAccountingConnection {
    /// Stable Mihomo connection identifier.
    #[serde(default)]
    pub id: String,
    /// Source, destination and process labels used by traffic reports.
    #[serde(default)]
    pub metadata: TrafficAccountingMetadata,
    /// Bytes uploaded by this connection.
    #[serde(default)]
    pub upload: u64,
    /// Bytes downloaded by this connection.
    #[serde(default)]
    pub download: u64,
    /// Mihomo-provided connection start timestamp.
    #[serde(default)]
    pub start: String,
    /// First outbound in the selected proxy chain.
    #[serde(
        default,
        rename = "chains",
        deserialize_with = "deserialize_first_string"
    )]
    pub outbound: String,
}

/// Minimal connection metadata retained for traffic reports.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TrafficAccountingMetadata {
    /// Source IP address.
    #[serde(default, rename = "sourceIP")]
    pub source_ip: String,
    /// Destination IP address used when no hostname is available.
    #[serde(default, rename = "destinationIP")]
    pub destination_ip: String,
    /// Sniffed destination hostname.
    #[serde(default)]
    pub host: String,
    /// Process name owning the connection, when available.
    #[serde(default)]
    pub process: String,
}

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

fn deserialize_connection_count<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<IgnoredAny>>::deserialize(deserializer)?
        .map_or(0, |connections| connections.len()))
}

fn deserialize_first_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct FirstStringVisitor;

    impl<'de> Visitor<'de> for FirstStringVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string array or null")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }

        fn visit_seq<A>(self, mut values: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let first = values.next_element()?.unwrap_or_default();
            while values.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
    }

    deserializer.deserialize_any(FirstStringVisitor)
}

#[cfg(test)]
mod tests {
    use super::{ConnectionsSummary, TrafficAccountingSnapshot};

    #[test]
    fn connection_summary_counts_entries_without_materializing_them() {
        let summary: ConnectionsSummary = serde_json::from_str(
            r#"{
                "connections": [
                    {"id":"first","metadata":{"host":"example.com"}},
                    {"id":"second","chains":["Proxy"]}
                ],
                "downloadTotal": 30,
                "uploadTotal": 20,
                "memory": 10
            }"#,
        )
        .unwrap();

        assert_eq!(
            summary,
            ConnectionsSummary {
                active_connections: 2,
                download_total: 30,
                upload_total: 20,
                memory: 10,
            }
        );
    }

    #[test]
    fn connection_summary_treats_null_connections_as_empty() {
        let summary: ConnectionsSummary =
            serde_json::from_str(r#"{"connections":null,"memory":10}"#).unwrap();

        assert_eq!(summary.active_connections, 0);
    }

    #[test]
    fn traffic_accounting_keeps_only_the_first_outbound_and_required_metadata() {
        let snapshot: TrafficAccountingSnapshot = serde_json::from_str(
            r#"{
                "connections": [{
                    "id":"first",
                    "metadata": {
                        "sourceIP":"127.0.0.1",
                        "destinationIP":"1.1.1.1",
                        "host":"example.com",
                        "process":"browser",
                        "processPath":"/unused/path",
                        "network":"tcp"
                    },
                    "upload":20,
                    "download":30,
                    "start":"2026-01-01T00:00:00Z",
                    "chains":["Proxy","Fallback"],
                    "rule":"MATCH",
                    "rulePayload":"unused"
                }],
                "downloadTotal":30,
                "uploadTotal":20,
                "memory":999
            }"#,
        )
        .unwrap();

        assert_eq!(snapshot.connections[0].outbound, "Proxy");
    }
}
