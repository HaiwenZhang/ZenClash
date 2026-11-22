//! Native Mihomo integration shared by the `ZenClash` user interface.

#![deny(missing_docs)]
// Nested configuration-transaction futures exceed rustc's default query depth in tests.
#![recursion_limit = "256"]

pub mod mihomo;
pub mod platform;
pub mod runtime;
pub mod storage;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod workflows;

/// Native service sessions, controller transport, authorization and runtime state.
pub use runtime::service::ipc as service;
/// Local AI network probe and reachability helpers.
pub use workflows::diagnostics::ai_network;

pub use mihomo::{
    AppPreferencesError, AppPreferencesResult, ConfigDiffEntry, ConfigDiffError, ConfigDiffKind,
    ConfigDiffReport, Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary,
    ControlledConfigError, ControlledConfigResult, CoreConfigValidationError, CoreConfigValidator,
    DEFAULT_NETWORK_LATENCY_TARGETS, DelayHistory, DelayResult, DnsAnswer, DnsQueryResponse,
    DnsQuestion, DnsRecordType, EffectiveConfigArtifact, EffectiveConfigPipeline,
    EffectiveConfigRequest, FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS, ListenerPortFallback,
    MemorySnapshot, MihomoEndpoint, MihomoError, MihomoResult, NativePropertyListError,
    NetworkLatencyTarget, NetworkProbeError, NetworkProbeResult, OverrideKeys, PacError,
    PacServerOperation, Provider, ProviderCatalog, ProxyCatalog, ProxyGroup, ProxyGroupBehavior,
    ProxyGroupSelectionObservation, ProxyNode, ProxyNodeId, PublicIpProvider, Rule, RuleCatalog,
    RuleRuntimeStats, RulesetBehavior, RulesetConversion, RulesetConversionError,
    RulesetConversionResult, RulesetConverter, RuntimeConfig, ServiceStartupRejection,
    SnifferConfig, SystemProxyBackendKind, SystemProxyBackendUnavailable, SystemProxyError,
    SystemProxyMode, SystemProxyOwnership, SystemProxySessionError, SystemProxySessionResult,
    SystemProxySetting, SystemProxyTransactionCause, SystemProxyValidationError,
    TrafficAccountingConnection, TrafficAccountingMetadata, TrafficAccountingSnapshot, TunConfig,
    TunPermissionError, TunPermissionResult, TunPermissionStatus, TunRuntimeObservation,
    TunRuntimeObserver, VersionInfo, apply_controlled_override, default_pac_script,
    default_system_proxy_bypass, diff_yaml_configs, merge_profile_overrides, normalize_pac_script,
    normalize_system_proxy_bypass, normalize_system_proxy_host, yaml_content_hash,
};
pub use platform::{
    AppDataPaths, AppInstanceLock, AppInstanceLockError, AutostartError, AutostartManager,
    AutostartResult, AutostartStatus, BundledResources, CapabilityState, CoreCapabilities,
    CoreKind, FirstRunStage, Observation, OperationalFailure, ParseCoreKindError,
    PlatformCommandError, PlatformCommandStage, PlatformCommandStream, RecoveryAction,
    WorkspaceResources, current_process_elevated,
};
pub use runtime::{
    BuiltinResourceUpdateReceipt, ControlledConfigStore, ControlledConfigUpdate, CoreApplyKind,
    CoreApplyOutcome, CoreBackupAdmission, CoreBinaryError, CoreBinaryInfo,
    CoreCommittedProfileSnapshot, CoreDataApplicationAdmission, CoreInitializationOutcome,
    CoreInstallOutcome, CoreLaunchConfig, CoreLaunchDescriptor, CoreLifecyclePhase,
    CoreLifecycleSnapshot, CoreLocalRecoveryOutcome, CoreMaintenanceIntent, CoreObservation,
    CoreObservationIdentity, CoreProcessLauncher, CoreProcessLease, CoreProcessSnapshot,
    CoreRestoreSnapshot, CoreRuntimeBackend, CoreRuntimeDescriptor, CoreSession, CoreSessionError,
    CoreSessionSnapshot, CoreTunPermissionStatus, CoreUpdateError, CoreUpdateResult,
    CoreUpdateTransaction, DesiredCoreIntent, EffectiveConfigIntent, IntentReconciler, KernelOwner,
    LauncherFuture, LeftoverLease, LeftoverLeaseTracker, LocalGeoDataRecovery, LocalLauncher,
    ManagedCoreProcess, MihomoClient, MihomoRelease, MihomoReleaseAsset, MihomoReleaseService,
    MihomoRuntimeResources, PreparedCoreUpdate, ReconcileOutcome, ServiceLauncher,
    ServiceRuntimeBundle, TrafficCaptureError, TrafficCaptureTask, bundled_default_profile,
    bundled_recovery_profile, check_sidecar_available, validate_core_binary,
    verify_ordinary_local_executable,
};
pub use storage::{
    AppPreferences, AppPreferencesObservation, AppPreferencesStore, AppearancePreference,
    CoreBinaryPreferences, DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS,
    DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES, DEFAULT_TRAFFIC_RETENTION_DAYS, LanguagePreference,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
    NetworkProbeRoutePreference, ProfileActivation, ProfileCatalog, ProfileRecord, ProfileSource,
    ProfileStore, ProfileStoreError, ProfileStoreResult, ProfileUpdate, RemoteProfileOptions,
    RemoteProfileRoute, SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage,
    TrafficAggregate, TrafficDeltaLogger, TrafficDimension, TrafficHistoryEntry,
    TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult, TrafficHistorySelection,
    TrafficHistorySnapshot, TrafficHistoryStore, TrafficOverview, TrafficTotals, TrafficTrendPoint,
    YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
    YamlOverrideStore, validate_clash_yaml,
};
pub use workflows::{
    AcceptedProxySelection, AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService,
    AppUpdateStatus, BackupDataSnapshot, BackupError, BackupExportSummary, BackupManager,
    BackupRestoreTransaction, BackupResult, CaptureOutcome, CapturePlan, CaptureStatus,
    ConnectionPolicy, ControllerCompatibility, ControllerStatus, CoreResourceAction,
    DiagnosticData, DiagnosticFailure, DiagnosticPlan, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, LIVE_TRAFFIC_HISTORY_LIMIT, LIVE_TRAFFIC_SAMPLE_COUNT,
    LogEntry, LogMonitor, LogPersistenceError, LogPersistenceResult, LogPersistenceStatus,
    LogStreamFormat, LogStreamSnapshot, LogTimeSource, MihomoLogLevel, NetworkDiagnostics,
    NetworkDiagnosticsError, NetworkDiagnosticsResult, NetworkLatencyResult, NetworkProbeRoute,
    NetworkProbeService, NetworkProbeSnapshot, NetworkReachability, ObservedBackupSnapshot,
    ObservedCapturePlan, ObservedPathRoute, ObservedProxySelection, OperationalSnapshot,
    OperationalStatus, OperationalStatusStream, PacProxyStatus, PacServer, PacServerStatus,
    PathStatus, PreparedBackupRestore, PreparedProfileChange, PreparedServiceBackupConfig,
    ProcessRecoveryStatus, ProcessStatus, ProfileApplication, ProfileApplicationError,
    ProfileApplyOutcome, ProfileChange, ProfilePreparationError, ProfileRecovery,
    ProfileSourceMetadata, ProfileVersion, ProviderActionReceipt, ProviderActionStatus,
    ProviderKind, ProviderOperationError, ProviderOperationFailure, ProviderOperationResult,
    ProviderOperationalStatus, ProviderOperations, ProviderUpdateBatchReceipt,
    ProviderUpdateTarget, ProxyDelayTarget, ProxyEndpointStatus, ProxyGroupActionReceipt,
    ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome, ProxySelectionReadback,
    ProxySelectionReceipt, ProxyVisibility, ProxyWorkspaceObservation, PublicIpInfo,
    ResourceActionReceipt, ServiceConfigOutcome, ServiceHealthKind, ServiceLocalRecoveryOutcome,
    ServiceMaintenancePreparation, ServiceMaintenanceRecoveryOutcome, ServiceMaintenanceRequest,
    ServiceManager, ServiceManagerError, ServiceManagerSnapshot, ServiceOperation, ServicePhase,
    ServiceTunOutcome, ServiceTunRequest, StreamStatus, StreamStatuses, SupportBundle, SupportSafe,
    SystemNetworkSnapshot, SystemProxyConfiguration, SystemProxyController, SystemProxyManager,
    SystemProxyOperation, SystemProxyOwnershipState, SystemProxyReconcileOutcome,
    SystemProxyReleaseReason, SystemProxySession, SystemProxySessionSnapshot, SystemProxySettings,
    SystemProxyStatus, SystemProxySummary, TimedTrafficSample, TrafficCaptureSession,
    TrafficCaptureSnapshot, TrafficMonitor, TrafficSample, TrafficSnapshot, TunCaptureStatus,
    configure_service_ipc, format_log_entries, format_log_entries_support_safe, format_speed,
    startup_service_health, startup_service_health_for, validate_external_https_url,
    write_log_export,
};

#[cfg(test)]
mod layer_facade_tests {
    use std::any::TypeId;

    use crate::{mihomo, platform, runtime, storage, workflows};

    #[test]
    fn five_layer_facade_exposes_expected_domain_types() {
        // Layer 0: platform
        let _ = TypeId::of::<platform::AppDataPaths>();
        let _ = TypeId::of::<platform::AppInstanceLock>();
        let _ = TypeId::of::<platform::AutostartManager>();
        let _ = TypeId::of::<platform::PlatformCommandError>();
        let _ = TypeId::of::<platform::Observation<()>>();
        let _ = TypeId::of::<platform::CoreKind>();

        // Layer 1: mihomo
        let _ = TypeId::of::<mihomo::CoreKind>();
        let _ = TypeId::of::<mihomo::MihomoEndpoint>();
        let _ = TypeId::of::<mihomo::EffectiveConfigPipeline>();
        let _ = TypeId::of::<mihomo::EffectiveConfigRequest>();
        let _ = TypeId::of::<mihomo::EffectiveConfigArtifact>();
        let _ = TypeId::of::<mihomo::OverrideKeys>();
        let _ = TypeId::of::<mihomo::RuntimeConfig>();
        let _ = TypeId::of::<mihomo::TunConfig>();
        let _ = TypeId::of::<mihomo::SnifferConfig>();
        let _ = TypeId::of::<mihomo::RulesetConverter>();
        assert!(
            mihomo::HOT_UPDATE_CONFIG_KEYS
                .iter()
                .all(|key| !mihomo::FULL_RELOAD_CONFIG_KEYS.contains(key))
        );
        assert_eq!(
            mihomo::yaml_content_hash("mode: rule\n"),
            crate::yaml_content_hash("mode: rule\n")
        );

        // Layer 2: storage
        let _ = TypeId::of::<storage::ProfileStore>();
        let _ = TypeId::of::<storage::YamlOverrideStore>();
        let _ = TypeId::of::<storage::AppPreferencesStore>();
        let _ = TypeId::of::<storage::TrafficHistoryStore>();
        const {
            assert!(storage::DEFAULT_TRAFFIC_RETENTION_DAYS > 0);
        }

        // Layer 3: runtime
        let _ = TypeId::of::<runtime::MihomoClient>();
        let _ = TypeId::of::<runtime::ControlledConfigStore>();
        let _ = TypeId::of::<runtime::CoreSession>();
        let _ = TypeId::of::<runtime::ManagedCoreProcess>();
        let _ = TypeId::of::<runtime::LocalLauncher>();
        let _ = TypeId::of::<runtime::ServiceLauncher>();
        let _ = TypeId::of::<runtime::IntentReconciler>();
        let _ = TypeId::of::<runtime::DesiredCoreIntent>();
        let _ = TypeId::of::<runtime::ReconcileOutcome>();
        let _ = TypeId::of::<runtime::LeftoverLeaseTracker>();
        let _ = TypeId::of::<runtime::KernelOwner>();
        let managed = runtime::LocalLauncher::managed(runtime::LeftoverLeaseTracker::new());
        assert_eq!(
            <runtime::LocalLauncher as runtime::CoreProcessLauncher>::backend_name(&managed),
            "local"
        );

        // Layer 4: workflows
        let _ = TypeId::of::<workflows::BackupManager>();
        let _ = TypeId::of::<workflows::ProfileApplication>();
        let _ = TypeId::of::<workflows::ProxyGroupActionReceipt<()>>();
        let _ = TypeId::of::<workflows::TrafficCaptureSession>();
        let _ = TypeId::of::<workflows::SystemProxySession>();
        let _ = TypeId::of::<workflows::ServiceManager>();
        let _ = TypeId::of::<workflows::ProxyOperations>();
        let _ = TypeId::of::<workflows::ProviderOperations>();
        let _ = TypeId::of::<workflows::TrafficMonitor>();
        let _ = TypeId::of::<workflows::LogMonitor>();
        let _ = TypeId::of::<workflows::OperationalStatus>();
        let _ = TypeId::of::<workflows::NetworkProbeService>();
        let _ = TypeId::of::<workflows::NetworkDiagnostics>();
        let _ = TypeId::of::<workflows::AppUpdateService>();
    }
}
