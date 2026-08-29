//! Typed response models for Mihomo's runtime controller APIs.

mod config;
mod connections;
mod dns;
mod providers;
mod rules;

pub use config::{RuntimeConfig, SnifferConfig, TunConfig};
pub use connections::{Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary};
pub use dns::{DnsAnswer, DnsQueryResponse, DnsQuestion, DnsRecordType};
pub use providers::{MemorySnapshot, Provider, ProviderCatalog};
pub use rules::{Rule, RuleCatalog, RuleRuntimeStats};

#[cfg(test)]
mod tests;
