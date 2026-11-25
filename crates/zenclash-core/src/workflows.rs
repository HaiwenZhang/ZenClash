//! Layer 4: High-level application workflows, capture orchestration, operations, backups, and telemetry.
//!
//! Groups transactional backup/restore, profile application, traffic capture & system proxy
//! sessions, privileged service orchestration, proxy & provider operations, live traffic/log/
//! operational-status telemetry, network probes & diagnostics, application update checks,
//! and AI network helpers.

pub(crate) mod backup;
pub(crate) mod capture;
pub(crate) mod diagnostics;
pub(crate) mod operations;
pub(crate) mod profile_apply;
pub(crate) mod service_manager;
pub(crate) mod telemetry;

pub use crate::mihomo::errors::{
    NativePropertyListError, PacError, PacServerOperation, SystemProxyBackendKind,
    SystemProxyBackendUnavailable, SystemProxyError, SystemProxySetting,
    SystemProxyTransactionCause, SystemProxyValidationError,
};
pub use crate::mihomo::models::{Provider, ProviderCatalog};
pub use crate::mihomo::proxy::{
    DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyGroupBehavior,
    ProxyGroupSelectionObservation, ProxyNode, ProxyNodeId,
};
pub use crate::mihomo::system_proxy_types::{
    SystemProxyMode, SystemProxyOwnership, default_pac_script, default_system_proxy_bypass,
    normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
};
pub use crate::platform::observation::{
    CapabilityState, FirstRunStage, Observation, OperationalFailure, RecoveryAction,
};
pub use crate::runtime::errors::{TrafficCaptureError, TrafficCaptureTask};
pub use backup::{
    BackupDataSnapshot, BackupError, BackupExportSummary, BackupManager, BackupRestoreTransaction,
    BackupResult, ObservedBackupSnapshot, PreparedBackupRestore,
};
pub use capture::{
    CaptureOutcome, CapturePlan, ObservedCapturePlan, PacProxyStatus, PacServer, PacServerStatus,
    ProxyEndpointStatus, ServiceLocalRecoveryOutcome, ServiceTunOutcome, SystemProxyConfiguration,
    SystemProxyController, SystemProxyManager, SystemProxyOperation, SystemProxyOwnershipState,
    SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession,
    SystemProxySessionError, SystemProxySessionResult, SystemProxySessionSnapshot,
    SystemProxySettings, SystemProxyStatus, SystemProxySummary, TrafficCaptureSession,
    TrafficCaptureSnapshot,
};
pub use diagnostics::{
    AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService, AppUpdateStatus,
    DEFAULT_NETWORK_LATENCY_TARGETS, DiagnosticData, DiagnosticFailure, DiagnosticPlan,
    DiagnosticReport, DiagnosticRoute, DiagnosticStep, DiagnosticStepKind, NetworkDiagnostics,
    NetworkDiagnosticsError, NetworkDiagnosticsResult, NetworkLatencyResult, NetworkLatencyTarget,
    NetworkProbeError, NetworkProbeResult, NetworkProbeRoute, NetworkProbeService,
    NetworkProbeSnapshot, NetworkReachability, PublicIpInfo, PublicIpProvider, SupportBundle,
    SupportSafe, SystemNetworkSnapshot, ai_network, validate_external_https_url,
};
pub use operations::{
    AcceptedProxySelection, ConnectionPolicy, CoreResourceAction, ObservedProxySelection,
    ProviderActionReceipt, ProviderActionStatus, ProviderKind, ProviderOperationError,
    ProviderOperationFailure, ProviderOperationResult, ProviderOperationalStatus,
    ProviderOperations, ProviderUpdateBatchReceipt, ProviderUpdateTarget, ProxyDelayTarget,
    ProxyGroupActionReceipt, ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome,
    ProxySelectionReadback, ProxySelectionReceipt, ProxyVisibility, ProxyWorkspaceObservation,
    ResourceActionReceipt,
};
pub use profile_apply::{
    PreparedProfileChange, ProfileApplication, ProfileApplicationError, ProfileApplyOutcome,
    ProfileChange, ProfilePreparationError, ProfileRecovery, ProfileSourceMetadata, ProfileVersion,
};
pub use service_manager::{
    PreparedServiceBackupConfig, ServiceConfigOutcome, ServiceHealthKind,
    ServiceMaintenancePreparation, ServiceMaintenanceRecoveryOutcome, ServiceMaintenanceRequest,
    ServiceManager, ServiceManagerError, ServiceManagerSnapshot, ServiceOperation, ServicePhase,
    ServiceTunRequest, configure_service_ipc, startup_service_health, startup_service_health_for,
    verify_ordinary_local_executable,
};
pub use telemetry::{
    CaptureStatus, ControllerCompatibility, ControllerStatus, LIVE_TRAFFIC_HISTORY_LIMIT,
    LIVE_TRAFFIC_SAMPLE_COUNT, LogEntry, LogMonitor, LogPersistenceError, LogPersistenceResult,
    LogPersistenceStatus, LogStreamFormat, LogStreamSnapshot, LogTimeSource, MihomoLogLevel,
    ObservedPathRoute, OperationalSnapshot, OperationalStatus, OperationalStatusStream, PathStatus,
    ProcessRecoveryStatus, ProcessStatus, StreamStatus, StreamStatuses, TimedTrafficSample,
    TrafficMonitor, TrafficSample, TrafficSnapshot, TunCaptureStatus, format_log_entries,
    format_log_entries_support_safe, format_speed, write_log_export,
};
