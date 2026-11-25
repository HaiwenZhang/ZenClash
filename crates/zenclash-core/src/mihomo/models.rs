//! Typed response models for Mihomo's runtime controller APIs.

use serde::Deserialize as _;

mod config;
mod connections;
mod dns;
mod providers;
mod rules;

pub use config::{RuntimeConfig, SnifferConfig, TunConfig};
pub use connections::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary,
    TrafficAccountingConnection, TrafficAccountingMetadata, TrafficAccountingSnapshot,
};
pub use dns::{DnsAnswer, DnsQueryResponse, DnsQuestion, DnsRecordType};
pub use providers::{MemorySnapshot, Provider, ProviderCatalog};
pub use rules::{Rule, RuleCatalog, RuleRuntimeStats};

// Mihomo's Go APIs may encode an empty slice as null. Keep element/type
// validation intact while giving null the same meaning as an empty collection.
pub(crate) fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests;
