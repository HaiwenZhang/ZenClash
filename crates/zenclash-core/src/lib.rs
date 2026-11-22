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
mod config_pipeline;
mod controlled_config;
mod core_backend;
mod core_errors;
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
mod observation_types;
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
mod runtime_launcher;
mod session_errors;
#[cfg(test)]
mod stream_publication_test_support;
mod system_proxy;
mod system_proxy_types;
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

pub use app_update::{
    AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService, AppUpdateStatus,
    validate_external_https_url,
};
pub use autostart::{AutostartError, AutostartManager, AutostartResult, AutostartStatus};
pub use backup::{
    BackupDataSnapshot, BackupError, BackupExportSummary, BackupManager, BackupRestoreTransaction,
    BackupResult, ObservedBackupSnapshot, PreparedBackupRestore,
};
pub use client::MihomoClient;
pub use config_diff::{
    ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport, diff_yaml_configs,
};
pub use config_pipeline::{
    EffectiveConfigArtifact, EffectiveConfigPipeline, EffectiveConfigRequest,
    FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS, OverrideKeys, apply_controlled_override,
    yaml_content_hash,
};
pub use controlled_config::{
    ControlledConfigError, ControlledConfigResult, ControlledConfigStore, ControlledConfigUpdate,
    ListenerPortFallback,
};
pub use core_backend::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use core_errors::{
    AppPreferencesError, AppPreferencesResult, MihomoError, MihomoResult, NativePropertyListError,
    PacError, PacServerOperation, ServiceStartupRejection, SystemProxyBackendKind,
    SystemProxyBackendUnavailable, SystemProxyError, SystemProxySetting,
    SystemProxyTransactionCause, SystemProxyValidationError, VersionInfo,
};
pub use core_installation::{CoreBinaryError, CoreBinaryInfo, validate_core_binary};
pub use core_session::{
    BuiltinResourceUpdateReceipt, CoreApplyOutcome, CoreCommittedProfileSnapshot,
    CoreInitializationOutcome, CoreInstallOutcome, CoreLifecyclePhase, CoreLifecycleSnapshot,
    CoreLocalRecoveryOutcome, CoreMaintenanceIntent, CoreObservation, CoreObservationIdentity,
    CoreResourceAction, CoreRestoreSnapshot, CoreSession, CoreSessionSnapshot,
    EffectiveConfigIntent, ResourceActionReceipt,
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
    bundled_default_profile, bundled_recovery_profile,
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
pub use observation_types::{
    CapabilityState, FirstRunStage, Observation, OperationalFailure, RecoveryAction,
};
pub use operational_status::{
    CaptureStatus, ControllerCompatibility, ControllerStatus, ObservedPathRoute,
    OperationalSnapshot, OperationalStatus, OperationalStatusStream, PathStatus,
    ProcessRecoveryStatus, ProcessStatus, StreamStatus, StreamStatuses, TunCaptureStatus,
};
pub use owned_core::CoreTunPermissionStatus;
pub use platform_command::{PlatformCommandError, PlatformCommandStage, PlatformCommandStream};
pub use preferences::{
    AppPreferences, AppPreferencesObservation, AppPreferencesStore, AppearancePreference,
    CoreBinaryPreferences, DEFAULT_TRAFFIC_RETENTION_DAYS, LanguagePreference,
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
    ProxyGroupActionReceipt, ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome,
    ProxySelectionReadback, ProxySelectionReceipt, ProxyVisibility, ProxyWorkspaceObservation,
};
pub use ruleset::{
    RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
    RulesetConverter,
};
pub use runtime_launcher::{
    CoreApplyKind, CoreBackupAdmission, CoreDataApplicationAdmission, CoreLaunchDescriptor,
    CoreProcessLauncher, CoreProcessLease, CoreRuntimeBackend, CoreRuntimeDescriptor,
    DesiredCoreIntent, IntentReconciler, KernelOwner, LauncherFuture, LeftoverLease,
    LeftoverLeaseTracker, LocalLauncher, ReconcileOutcome, ServiceLauncher,
};
pub use session_errors::{CoreSessionError, TrafficCaptureError, TrafficCaptureTask};
pub use system_proxy::{
    PacProxyStatus, PacServer, PacServerStatus, ProxyEndpointStatus, SystemProxyConfiguration,
    SystemProxyController, SystemProxyManager, SystemProxyOperation, SystemProxyOwnershipState,
    SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession,
    SystemProxySessionError, SystemProxySessionResult, SystemProxySessionSnapshot,
    SystemProxySettings, SystemProxyStatus, SystemProxySummary,
};
pub use system_proxy_types::{
    SystemProxyMode, SystemProxyOwnership, default_pac_script, default_system_proxy_bypass,
    normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
};
pub use traffic::{
    LIVE_TRAFFIC_HISTORY_LIMIT, LIVE_TRAFFIC_SAMPLE_COUNT, TimedTrafficSample, TrafficMonitor,
    TrafficSample, TrafficSnapshot, format_speed,
};
pub use traffic_capture::{
    CaptureOutcome, CapturePlan, ObservedCapturePlan, ServiceLocalRecoveryOutcome,
    ServiceTunOutcome, TrafficCaptureSession, TrafficCaptureSnapshot,
};
pub use traffic_history::{
    TrafficAggregate, TrafficDeltaLogger, TrafficDimension, TrafficHistoryEntry,
    TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult, TrafficHistorySelection,
    TrafficHistorySnapshot, TrafficHistoryStore, TrafficOverview, TrafficTotals, TrafficTrendPoint,
};
pub use tun_permissions::{TunPermissionError, TunPermissionResult, TunPermissionStatus};
pub use tun_runtime::{TunRuntimeObservation, TunRuntimeObserver};
pub use yaml_overrides::{
    YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
    YamlOverrideStore,
};

impl LocalLauncher {
    /// Creates a concrete zero-privilege local launcher backed by
    /// [`ManagedCoreProcess::spawn`] executed on a blocking worker thread.
    #[must_use]
    pub fn managed(leftovers: LeftoverLeaseTracker) -> Self {
        Self::with_blocking(leftovers, |descriptor| {
            let process = ManagedCoreProcess::spawn(CoreLaunchConfig::from(descriptor))?;
            Ok(process as std::sync::Arc<dyn CoreProcessLease>)
        })
    }
}

/// Layer 0: Cross-platform OS infrastructure primitives with zero domain dependencies.
///
/// Groups application data paths, bundled resources, single-instance process locks,
/// OS autostart integration, and bounded non-blocking command execution.
pub mod platform {
    pub use crate::application_paths::{AppDataPaths, BundledResources, WorkspaceResources};
    pub use crate::autostart::{
        AutostartError, AutostartManager, AutostartResult, AutostartStatus,
    };
    pub use crate::instance_lock::{AppInstanceLock, AppInstanceLockError};
    pub use crate::platform_command::{
        PlatformCommandError, PlatformCommandStage, PlatformCommandStream,
    };
}

/// Layer 1: Mihomo protocol, typed models, stateless config pipeline, and TUN/ruleset utilities.
///
/// Groups controller endpoints, HTTP/IPC transport client, single-`Value` effective
/// configuration synthesis, YAML diff/merge/validation, read-only TUN runtime
/// observation, TUN permission status, ruleset conversion, and controller response models.
pub mod mihomo {
    pub use crate::client::MihomoClient;
    pub use crate::config_diff::{
        ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport, diff_yaml_configs,
    };
    pub use crate::config_pipeline::{
        EffectiveConfigArtifact, EffectiveConfigPipeline, EffectiveConfigRequest,
        FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS, OverrideKeys, apply_controlled_override,
        yaml_content_hash,
    };
    pub use crate::core_errors::{MihomoError, MihomoResult, ServiceStartupRejection, VersionInfo};
    pub use crate::core_validation::{CoreConfigValidationError, CoreConfigValidator};
    pub use crate::endpoint::MihomoEndpoint;
    pub use crate::models::{
        Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary, DnsAnswer,
        DnsQueryResponse, DnsQuestion, DnsRecordType, MemorySnapshot, Provider, ProviderCatalog,
        Rule, RuleCatalog, RuleRuntimeStats, RuntimeConfig, SnifferConfig,
        TrafficAccountingConnection, TrafficAccountingMetadata, TrafficAccountingSnapshot,
        TunConfig,
    };
    pub use crate::profile::merge_profile_overrides;
    pub use crate::ruleset::{
        RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
        RulesetConverter,
    };
    pub use crate::tun_permissions::{
        TunPermissionError, TunPermissionResult, TunPermissionStatus,
    };
    pub use crate::tun_runtime::{TunRuntimeObservation, TunRuntimeObserver};
}

/// Layer 2: Persistent repositories, stores, preferences, traffic history, and backups.
///
/// Groups managed profile storage, YAML override chains, controlled runtime config
/// storage, application preferences, SQLite traffic history, and transactional backups.
pub mod storage {
    pub use crate::backup::{
        BackupDataSnapshot, BackupError, BackupExportSummary, BackupManager,
        BackupRestoreTransaction, BackupResult, ObservedBackupSnapshot, PreparedBackupRestore,
    };
    pub use crate::controlled_config::{
        ControlledConfigError, ControlledConfigResult, ControlledConfigStore,
        ControlledConfigUpdate, ListenerPortFallback,
    };
    pub use crate::core_errors::{AppPreferencesError, AppPreferencesResult};
    pub use crate::preferences::{
        AppPreferences, AppPreferencesObservation, AppPreferencesStore, AppearancePreference,
        CoreBinaryPreferences, DEFAULT_TRAFFIC_RETENTION_DAYS, LanguagePreference,
        NetworkProbeRoutePreference,
    };
    pub use crate::profiles::{
        DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
        MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
        MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
        PreparedProfileChange, ProfileActivation, ProfileApplication, ProfileApplicationError,
        ProfileApplyOutcome, ProfileCatalog, ProfileChange, ProfilePreparationError, ProfileRecord,
        ProfileRecovery, ProfileSource, ProfileSourceMetadata, ProfileStore, ProfileStoreError,
        ProfileStoreResult, ProfileUpdate, ProfileVersion, RemoteProfileOptions,
        RemoteProfileRoute, SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage,
        validate_clash_yaml,
    };
    pub use crate::traffic_history::{
        TrafficAggregate, TrafficDeltaLogger, TrafficDimension, TrafficHistoryEntry,
        TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult, TrafficHistorySelection,
        TrafficHistorySnapshot, TrafficHistoryStore, TrafficOverview, TrafficTotals,
        TrafficTrendPoint,
    };
    pub use crate::yaml_overrides::{
        YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
        YamlOverrideStore,
    };
}

/// Layer 3: Managed core lifecycle, unified process launchers, ownership leases, and `CoreSession`.
///
/// Groups the unified `CoreProcessLauncher`/`CoreProcessLease` abstractions, concrete
/// `LocalLauncher`/`ServiceLauncher`, the latest-desired-intent `IntentReconciler`
/// and `LeftoverLeaseTracker`, core binary validation/update, `ManagedCoreProcess`,
/// and `CoreSession`.
pub mod runtime {
    pub use crate::core_backend::{CoreCapabilities, CoreKind, ParseCoreKindError};
    pub use crate::core_installation::{CoreBinaryError, CoreBinaryInfo, validate_core_binary};
    pub use crate::core_session::{
        BuiltinResourceUpdateReceipt, CoreApplyOutcome, CoreCommittedProfileSnapshot,
        CoreInitializationOutcome, CoreInstallOutcome, CoreLifecyclePhase, CoreLifecycleSnapshot,
        CoreLocalRecoveryOutcome, CoreMaintenanceIntent, CoreObservation, CoreObservationIdentity,
        CoreResourceAction, CoreRestoreSnapshot, CoreSession, CoreSessionSnapshot,
        EffectiveConfigIntent, ResourceActionReceipt,
    };
    pub use crate::core_update::{
        CoreUpdateError, CoreUpdateResult, CoreUpdateTransaction, MihomoRelease,
        MihomoReleaseAsset, MihomoReleaseService, PreparedCoreUpdate,
    };
    pub use crate::managed_core_process::{
        CoreLaunchConfig, CoreProcessSnapshot, ManagedCoreProcess, MihomoRuntimeResources,
        bundled_default_profile, bundled_recovery_profile,
    };
    pub use crate::owned_core::CoreTunPermissionStatus;
    pub use crate::proxy_operations::ProxyGroupActionReceipt;
    pub use crate::runtime_launcher::{
        CoreApplyKind, CoreBackupAdmission, CoreDataApplicationAdmission, CoreLaunchDescriptor,
        CoreProcessLauncher, CoreProcessLease, CoreRuntimeBackend, CoreRuntimeDescriptor,
        DesiredCoreIntent, IntentReconciler, KernelOwner, LauncherFuture, LeftoverLease,
        LeftoverLeaseTracker, LocalLauncher, ReconcileOutcome, ServiceLauncher,
    };
    pub use crate::service::{check_sidecar_available, current_process_elevated};
    pub use crate::service_runtime::{LocalGeoDataRecovery, ServiceRuntimeBundle};
    pub use crate::session_errors::CoreSessionError;
}

/// Layer 4: High-level application workflows, capture orchestration, operations, and telemetry.
///
/// Groups traffic capture & system proxy sessions, privileged service orchestration,
/// proxy & provider operations, live traffic/log/operational-status telemetry,
/// network probes & diagnostics, application update checks, and AI network helpers.
pub mod workflows {
    pub use crate::ai_network;
    pub use crate::app_update::{
        AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService, AppUpdateStatus,
        validate_external_https_url,
    };
    pub use crate::core_errors::{
        NativePropertyListError, PacError, PacServerOperation, SystemProxyBackendKind,
        SystemProxyBackendUnavailable, SystemProxyError, SystemProxySetting,
        SystemProxyTransactionCause, SystemProxyValidationError,
    };
    pub use crate::logs::{
        LogEntry, LogMonitor, LogPersistenceError, LogPersistenceResult, LogPersistenceStatus,
        LogStreamFormat, LogStreamSnapshot, LogTimeSource, MihomoLogLevel, format_log_entries,
        format_log_entries_support_safe, write_log_export,
    };
    pub use crate::models::{Provider, ProviderCatalog};
    pub use crate::network::{
        DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyResult, NetworkLatencyTarget,
        NetworkProbeError, NetworkProbeResult, NetworkProbeRoute, NetworkProbeService,
        NetworkProbeSnapshot, NetworkReachability, PublicIpInfo, PublicIpProvider,
        SystemNetworkSnapshot,
    };
    pub use crate::network_diagnostics::{
        DiagnosticData, DiagnosticFailure, DiagnosticPlan, DiagnosticReport, DiagnosticRoute,
        DiagnosticStep, DiagnosticStepKind, NetworkDiagnostics, NetworkDiagnosticsError,
        NetworkDiagnosticsResult, SupportBundle, SupportSafe,
    };
    pub use crate::observation_types::{
        CapabilityState, FirstRunStage, Observation, OperationalFailure, RecoveryAction,
    };
    pub use crate::operational_status::{
        CaptureStatus, ControllerCompatibility, ControllerStatus, ObservedPathRoute,
        OperationalSnapshot, OperationalStatus, OperationalStatusStream, PathStatus,
        ProcessRecoveryStatus, ProcessStatus, StreamStatus, StreamStatuses, TunCaptureStatus,
    };
    pub use crate::provider_operations::{
        ProviderActionReceipt, ProviderActionStatus, ProviderKind, ProviderOperationError,
        ProviderOperationFailure, ProviderOperationResult, ProviderOperationalStatus,
        ProviderOperations, ProviderUpdateBatchReceipt, ProviderUpdateTarget,
    };
    pub use crate::proxy::{
        DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyGroupBehavior,
        ProxyGroupSelectionObservation, ProxyNode, ProxyNodeId,
    };
    pub use crate::proxy_operations::{
        AcceptedProxySelection, ConnectionPolicy, ObservedProxySelection, ProxyDelayTarget,
        ProxyGroupActionReceipt, ProxyGroupMeasurementOutcome, ProxyOperations,
        ProxySelectionOutcome, ProxySelectionReadback, ProxySelectionReceipt, ProxyVisibility,
        ProxyWorkspaceObservation,
    };
    pub use crate::service_manager::{
        PreparedServiceBackupConfig, ServiceConfigOutcome, ServiceHealthKind,
        ServiceMaintenancePreparation, ServiceMaintenanceRecoveryOutcome,
        ServiceMaintenanceRequest, ServiceManager, ServiceManagerError, ServiceManagerSnapshot,
        ServiceOperation, ServicePhase, ServiceTunRequest, configure_service_ipc,
        startup_service_health, startup_service_health_for, verify_ordinary_local_executable,
    };
    pub use crate::session_errors::{TrafficCaptureError, TrafficCaptureTask};
    pub use crate::system_proxy::{
        PacProxyStatus, PacServer, PacServerStatus, ProxyEndpointStatus, SystemProxyConfiguration,
        SystemProxyController, SystemProxyManager, SystemProxyOperation, SystemProxyOwnershipState,
        SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession,
        SystemProxySessionError, SystemProxySessionResult, SystemProxySessionSnapshot,
        SystemProxySettings, SystemProxyStatus, SystemProxySummary,
    };
    pub use crate::system_proxy_types::{
        SystemProxyMode, SystemProxyOwnership, default_pac_script, default_system_proxy_bypass,
        normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
    };
    pub use crate::traffic::{
        LIVE_TRAFFIC_HISTORY_LIMIT, LIVE_TRAFFIC_SAMPLE_COUNT, TimedTrafficSample, TrafficMonitor,
        TrafficSample, TrafficSnapshot, format_speed,
    };
    pub use crate::traffic_capture::{
        CaptureOutcome, CapturePlan, ObservedCapturePlan, ServiceLocalRecoveryOutcome,
        ServiceTunOutcome, TrafficCaptureSession, TrafficCaptureSnapshot,
    };
}

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

        // Layer 1: mihomo
        let _ = TypeId::of::<mihomo::MihomoEndpoint>();
        let _ = TypeId::of::<mihomo::MihomoClient>();
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
        let _ = TypeId::of::<storage::ControlledConfigStore>();
        let _ = TypeId::of::<storage::AppPreferencesStore>();
        let _ = TypeId::of::<storage::TrafficHistoryStore>();
        let _ = TypeId::of::<storage::BackupManager>();
        const {
            assert!(storage::DEFAULT_TRAFFIC_RETENTION_DAYS > 0);
        }

        // Layer 3: runtime
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
