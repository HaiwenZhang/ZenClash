//! Native Mihomo integration shared by the `ZenClash` user interface.

#![deny(missing_docs)]

mod autostart;
mod backup;
mod client;
mod config_diff;
mod controlled_config;
mod core_backend;
mod core_installation;
mod core_update;
mod endpoint;
mod logs;
mod models;
mod network;
mod platform_command;
mod preferences;
mod process;
mod profile;
mod profiles;
mod proxy;
mod ruleset;
mod substore;
mod system_proxy;
mod traffic;
mod traffic_history;
mod tun_permissions;
mod webdav;
mod websocket;
mod yaml_overrides;

pub use autostart::{AutostartError, AutostartManager, AutostartResult, AutostartStatus};
pub use backup::{
    BackupError, BackupExportSummary, BackupManager, BackupRestoreTransaction, BackupResult,
    PreparedBackupRestore,
};
pub use client::{MihomoClient, MihomoError, MihomoResult, VersionInfo};
pub use config_diff::{
    diff_yaml_configs, ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport,
};
pub use controlled_config::{
    ControlledConfigError, ControlledConfigResult, ControlledConfigStore, ControlledConfigUpdate,
};
pub use core_backend::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use core_installation::{validate_core_binary, CoreBinaryError, CoreBinaryInfo};
pub use core_update::{
    CoreUpdateError, CoreUpdateResult, CoreUpdateTransaction, MihomoRelease, MihomoReleaseAsset,
    MihomoReleaseService, PreparedCoreUpdate,
};
pub use endpoint::MihomoEndpoint;
pub use logs::{
    format_log_entries, LogEntry, LogMonitor, LogPersistenceError, LogPersistenceResult,
    LogPersistenceStatus, MihomoLogLevel,
};
pub use models::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, MemorySnapshot, Provider, ProviderCatalog,
    Rule, RuleCatalog, RuleRuntimeStats, RuntimeConfig, SnifferConfig, TunConfig,
};
pub use network::{
    NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeError, NetworkProbeResult,
    NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot, PublicIpInfo, PublicIpProvider,
    SystemNetworkSnapshot, DEFAULT_NETWORK_LATENCY_TARGETS,
};
pub use preferences::{
    AppPreferences, AppPreferencesError, AppPreferencesResult, AppPreferencesStore,
    AppearancePreference, CoreBinaryPreferences, NetworkProbeRoutePreference,
};
pub use process::{MihomoLaunchConfig, MihomoProcess, MihomoProcessSnapshot};
pub use profile::merge_profile_overrides;
pub use profiles::{
    validate_clash_yaml, ProfileActivation, ProfileCatalog, ProfileRecord, ProfileSource,
    ProfileStore, ProfileStoreError, ProfileStoreResult, ProfileUpdate, RemoteProfileOptions,
    RemoteProfileRoute, SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage,
    DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
};
pub use proxy::{DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyNode};
pub use ruleset::{
    RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
    RulesetConverter,
};
pub use substore::{SubStoreClient, SubStoreItem, SubStoreItemKind, SubStoreSnapshot};
pub use system_proxy::{
    default_pac_script, default_system_proxy_bypass, normalize_pac_script,
    normalize_system_proxy_bypass, normalize_system_proxy_host, PacServer, PacServerStatus,
    SystemProxyController, SystemProxyManager, SystemProxyMode, SystemProxyStatus,
};
pub use traffic::{format_speed, TrafficMonitor, TrafficSnapshot};
pub use traffic_history::{
    TrafficAggregate, TrafficDeltaLogger, TrafficDimension, TrafficHistoryEntry,
    TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult, TrafficHistoryStore,
    TrafficOverview, TrafficTotals, TrafficTrendPoint, DEFAULT_TRAFFIC_RETENTION_DAYS,
};
pub use tun_permissions::{
    TunPermissionError, TunPermissionGrant, TunPermissionManager, TunPermissionResult,
    TunPermissionStatus,
};
pub use webdav::{
    WebDavBackup, WebDavError, WebDavResult, WebDavService, WebDavSettings, WebDavSettingsStore,
    WebDavUploadSummary,
};
pub use yaml_overrides::{
    YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
    YamlOverrideStore,
};
