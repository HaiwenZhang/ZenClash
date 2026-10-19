//! Native Mihomo integration shared by the `ZenClash` user interface.

#![deny(missing_docs)]
// Nested configuration-transaction futures exceed rustc's default query depth in tests.
#![recursion_limit = "256"]

pub use crate::service::{check_sidecar_available, current_process_elevated};

pub mod ai_network;
mod app_update;
mod application_paths;
pub use application_paths::{AppDataPaths, BundledResources, WorkspaceResources};
mod autostart;
mod backup;
mod client;
/// Native service sessions, controller transport, authorization and runtime state.
pub mod service;
mod service_manager;
mod service_runtime;
mod service_runtime_session;

pub use service_manager::{
    PreparedServiceBackupConfig, ServiceConfigOutcome, ServiceHealthKind,
    ServiceMaintenancePreparation, ServiceMaintenanceRecoveryOutcome, ServiceMaintenanceRequest,
    ServiceManager, ServiceManagerError, ServiceManagerSnapshot, ServiceOperation, ServicePhase,
    ServiceTunRequest, configure_service_ipc, startup_service_health, startup_service_health_for,
    verify_ordinary_local_executable,
};

pub use service_runtime::{LocalGeoDataRecovery, ServiceRuntimeBundle};
mod config_diff;
mod controlled_config;
mod core_backend;
mod core_installation;
mod core_session;
mod core_update;
mod core_validation;
mod data_coordinator;
mod endpoint;
mod http_body;
mod instance_lock;
mod listener_fallback;
mod logs;
mod managed_core_process;
mod models;
mod network;
mod network_diagnostics;
mod operational_status;
mod owned_core;
mod platform_command;
mod preferences;
mod profile;
mod profiles;
mod provider_operations;
mod proxy;
mod proxy_operations;
mod ruleset;
#[cfg(test)]
mod stream_publication_test_support;
mod system_proxy;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod traffic;
mod traffic_capture;
mod traffic_history;
mod tun_admission;
mod tun_config;
mod tun_permissions;
mod tun_runtime;
mod websocket;
mod yaml_overrides;

/// Compatibility name for the launch inputs shared by managed core implementations.
pub use CoreLaunchConfig as MihomoLaunchConfig;
/// Compatibility name for a managed core process snapshot.
pub use CoreProcessSnapshot as MihomoProcessSnapshot;
/// Compatibility name for the managed Mihomo process.
pub use ManagedCoreProcess as MihomoProcess;
pub use app_update::{
    AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService, AppUpdateStatus,
    validate_external_https_url,
};
pub use autostart::{AutostartError, AutostartManager, AutostartResult, AutostartStatus};
pub use backup::{
    BackupDataSnapshot, BackupError, BackupExportSummary, BackupManager, BackupRestoreTransaction,
    BackupResult, ObservedBackupSnapshot, PreparedBackupRestore,
};
pub use client::{MihomoClient, MihomoError, MihomoResult, ServiceStartupRejection, VersionInfo};
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
    BuiltinResourceUpdateReceipt, CoreApplyKind, CoreApplyOutcome, CoreBackupAdmission,
    CoreCommittedProfileSnapshot, CoreDataApplicationAdmission, CoreInitializationOutcome,
    CoreInstallOutcome, CoreLifecyclePhase, CoreLifecycleSnapshot, CoreLocalRecoveryOutcome,
    CoreMaintenanceIntent, CoreObservation, CoreObservationIdentity, CoreResourceAction,
    CoreRestoreSnapshot, CoreSession, CoreSessionError, CoreSessionSnapshot, EffectiveConfigIntent,
    ProxyGroupActionReceipt, ResourceActionReceipt,
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
    LogStreamFormat, LogStreamSnapshot, LogTimeSource, MihomoLogLevel, format_log_entries,
    format_log_entries_support_safe, write_log_export,
};
pub use managed_core_process::{
    CoreLaunchConfig, CoreProcessSnapshot, ManagedCoreProcess, MihomoRuntimeResources,
    bundled_recovery_profile,
};
pub use models::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary, DnsAnswer,
    DnsQueryResponse, DnsQuestion, DnsRecordType, MemorySnapshot, Provider, ProviderCatalog, Rule,
    RuleCatalog, RuleRuntimeStats, RuntimeConfig, SnifferConfig, TrafficAccountingConnection,
    TrafficAccountingMetadata, TrafficAccountingSnapshot, TunConfig,
};
pub use network::{
    DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeError,
    NetworkProbeResult, NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot,
    NetworkReachability, PublicIpInfo, PublicIpProvider, SystemNetworkSnapshot,
};
pub use network_diagnostics::{
    DiagnosticData, DiagnosticFailure, DiagnosticPlan, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, NetworkDiagnostics, NetworkDiagnosticsError,
    NetworkDiagnosticsResult, SupportBundle, SupportSafe,
};
pub use operational_status::{
    CapabilityState, CaptureStatus, ControllerCompatibility, ControllerStatus, FirstRunStage,
    Observation, ObservedPathRoute, OperationalFailure, OperationalSnapshot, OperationalStatus,
    OperationalStatusStream, PathStatus, ProcessRecoveryStatus, ProcessStatus, RecoveryAction,
    StreamStatus, StreamStatuses, TunCaptureStatus,
};
pub use owned_core::{CoreRuntimeBackend, CoreRuntimeDescriptor, CoreTunPermissionStatus};
pub use platform_command::{PlatformCommandError, PlatformCommandStage, PlatformCommandStream};
pub use preferences::{
    AppPreferences, AppPreferencesError, AppPreferencesObservation, AppPreferencesResult,
    AppPreferencesStore, AppearancePreference, CoreBinaryPreferences, LanguagePreference,
    NetworkProbeRoutePreference,
};

pub use profile::merge_profile_overrides;
pub use profiles::{
    DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
    PreparedProfileChange, ProfileActivation, ProfileApplication, ProfileApplicationError,
    ProfileApplyOutcome, ProfileCatalog, ProfileChange, ProfilePreparationError, ProfileRecord,
    ProfileRecovery, ProfileSource, ProfileSourceMetadata, ProfileStore, ProfileStoreError,
    ProfileStoreResult, ProfileUpdate, ProfileVersion, RemoteProfileOptions, RemoteProfileRoute,
    SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage, validate_clash_yaml,
};
pub use provider_operations::{
    ProviderActionReceipt, ProviderActionStatus, ProviderKind, ProviderOperationError,
    ProviderOperationFailure, ProviderOperationResult, ProviderOperationalStatus,
    ProviderOperations, ProviderUpdateBatchReceipt, ProviderUpdateTarget,
};
pub use proxy::{
    DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyGroupBehavior,
    ProxyGroupSelectionObservation, ProxyNode, ProxyNodeId,
};
pub use proxy_operations::{
    AcceptedProxySelection, ConnectionPolicy, ObservedProxySelection, ProxyDelayTarget,
    ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome, ProxySelectionReadback,
    ProxySelectionReceipt, ProxyVisibility, ProxyWorkspaceObservation,
};
pub use ruleset::{
    RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
    RulesetConverter,
};
pub use system_proxy::{
    NativePropertyListError, PacError, PacProxyStatus, PacServer, PacServerOperation,
    PacServerStatus, ProxyEndpointStatus, SystemProxyBackendKind, SystemProxyBackendUnavailable,
    SystemProxyConfiguration, SystemProxyController, SystemProxyError, SystemProxyManager,
    SystemProxyMode, SystemProxyOperation, SystemProxyOwnership, SystemProxyOwnershipState,
    SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession,
    SystemProxySessionError, SystemProxySessionResult, SystemProxySessionSnapshot,
    SystemProxySetting, SystemProxySettings, SystemProxyStatus, SystemProxySummary,
    SystemProxyTransactionCause, SystemProxyValidationError, default_pac_script,
    default_system_proxy_bypass, normalize_pac_script, normalize_system_proxy_bypass,
    normalize_system_proxy_host,
};
pub use traffic::{
    LIVE_TRAFFIC_HISTORY_LIMIT, LIVE_TRAFFIC_SAMPLE_COUNT, TimedTrafficSample, TrafficMonitor,
    TrafficSample, TrafficSnapshot, format_speed,
};
pub use traffic_capture::{
    CaptureOutcome, CapturePlan, ObservedCapturePlan, ServiceLocalRecoveryOutcome,
    ServiceTunOutcome, TrafficCaptureError, TrafficCaptureSession, TrafficCaptureSnapshot,
    TrafficCaptureTask,
};
pub use traffic_history::{
    DEFAULT_TRAFFIC_RETENTION_DAYS, TrafficAggregate, TrafficDeltaLogger, TrafficDimension,
    TrafficHistoryEntry, TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult,
    TrafficHistorySelection, TrafficHistorySnapshot, TrafficHistoryStore, TrafficOverview,
    TrafficTotals, TrafficTrendPoint,
};
pub use tun_permissions::{TunPermissionError, TunPermissionResult, TunPermissionStatus};
pub use tun_runtime::{TunRuntimeObservation, TunRuntimeObserver};
pub use yaml_overrides::{
    YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
    YamlOverrideStore,
};
