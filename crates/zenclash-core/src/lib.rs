//! Native Mihomo integration shared by the `ZenClash` user interface.

#![deny(missing_docs)]

mod autostart;
mod backup;
mod client;
mod config_diff;
mod controlled_config;
mod core_backend;
mod core_installation;
mod core_session;
mod core_update;
mod core_validation;
mod endpoint;
mod instance_lock;
mod listener_fallback;
mod logs;
mod models;
mod network;
mod platform_command;
mod preferences;
mod process;
mod profile;
mod profiles;
mod proxy;
mod proxy_operations;
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
    ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport, diff_yaml_configs,
};
pub use controlled_config::{
    ControlledConfigError, ControlledConfigResult, ControlledConfigStore, ControlledConfigUpdate,
    ListenerPortFallback,
};
pub use core_backend::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use core_installation::{CoreBinaryError, CoreBinaryInfo, validate_core_binary};
pub use core_session::{
    CoreApplyKind, CoreApplyOutcome, CoreMaintenanceIntent, CoreSession, CoreSessionError,
    CoreSessionSnapshot, EffectiveConfigIntent,
};
pub use core_update::{
    CoreUpdateError, CoreUpdateResult, CoreUpdateTransaction, MihomoRelease, MihomoReleaseAsset,
    MihomoReleaseService, PreparedCoreUpdate,
};
pub use core_validation::{CoreConfigValidationError, CoreConfigValidator};
pub use endpoint::MihomoEndpoint;
pub use instance_lock::{AppInstanceLock, AppInstanceLockError};
pub use logs::{
    LogEntry, LogMonitor, LogPersistenceError, LogPersistenceResult, LogPersistenceStatus,
    MihomoLogLevel, format_log_entries,
};
pub use models::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, MemorySnapshot, Provider, ProviderCatalog,
    Rule, RuleCatalog, RuleRuntimeStats, RuntimeConfig, SnifferConfig, TunConfig,
};
pub use network::{
    DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeError,
    NetworkProbeResult, NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot, PublicIpInfo,
    PublicIpProvider, SystemNetworkSnapshot,
};
pub use preferences::{
    AppPreferences, AppPreferencesError, AppPreferencesResult, AppPreferencesStore,
    AppearancePreference, CoreBinaryPreferences, LanguagePreference, NetworkProbeRoutePreference,
};
pub use process::{
    MihomoLaunchConfig, MihomoProcess, MihomoProcessSnapshot, bundled_recovery_profile,
};
pub use profile::merge_profile_overrides;
pub use profiles::{
    DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES, ProfileActivation,
    ProfileCatalog, ProfileRecord, ProfileSource, ProfileStore, ProfileStoreError,
    ProfileStoreResult, ProfileUpdate, RemoteProfileOptions, RemoteProfileRoute,
    SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage, validate_clash_yaml,
};
pub use proxy::{DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyNode};
pub use proxy_operations::{ProxyDelayTarget, ProxyOperations, ProxySelectionOutcome};
pub use ruleset::{
    RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
    RulesetConverter,
};
pub use substore::{SubStoreClient, SubStoreItem, SubStoreItemKind, SubStoreSnapshot};
pub use system_proxy::{
    PacServer, PacServerStatus, SystemProxyController, SystemProxyManager, SystemProxyMode,
    SystemProxyOperation, SystemProxyOwnership, SystemProxyReconcileOutcome,
    SystemProxyReleaseReason, SystemProxySession, SystemProxySessionError,
    SystemProxySessionResult, SystemProxySettings, SystemProxyStatus, default_pac_script,
    default_system_proxy_bypass, normalize_pac_script, normalize_system_proxy_bypass,
    normalize_system_proxy_host,
};
pub use traffic::{
    LIVE_TRAFFIC_SAMPLE_COUNT, TrafficMonitor, TrafficSample, TrafficSnapshot, format_speed,
};
pub use traffic_history::{
    DEFAULT_TRAFFIC_RETENTION_DAYS, TrafficAggregate, TrafficDeltaLogger, TrafficDimension,
    TrafficHistoryEntry, TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult,
    TrafficHistoryStore, TrafficOverview, TrafficTotals, TrafficTrendPoint,
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
