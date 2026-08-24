//! Native Mihomo integration shared by the ZenClash user interface.

mod client;
mod endpoint;
mod logs;
mod models;
mod network;
mod process;
mod profile;
mod proxy;
mod substore;
mod system_proxy;
mod traffic;

pub use client::{MihomoClient, MihomoError, MihomoResult, VersionInfo};
pub use endpoint::MihomoEndpoint;
pub use logs::{LogEntry, LogMonitor};
pub use models::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, MemorySnapshot, Provider, ProviderCatalog,
    Rule, RuleCatalog, RuntimeConfig, SnifferConfig, TunConfig,
};
pub use network::SystemNetworkSnapshot;
pub use process::{MihomoLaunchConfig, MihomoProcess, MihomoProcessSnapshot};
pub use profile::merge_profile_overrides;
pub use proxy::{DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyNode};
pub use substore::{SubStoreClient, SubStoreItem, SubStoreSnapshot};
pub use system_proxy::{SystemProxyManager, SystemProxyStatus};
pub use traffic::{format_speed, TrafficMonitor, TrafficSnapshot};
