//! Typed failures crossing the capture coordinator's backend and core seams.

use crate::{CoreSessionError, MihomoError, SystemProxySessionError, YamlOverrideError};
use thiserror::Error;

/// Background task whose capture operation could not complete normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrafficCaptureTask {
    /// Completing an admitted capture transaction.
    Completion,
    /// Loading the enabled override chain.
    LoadOverrides,
    /// Reading native proxy intent and state.
    ReadSystemProxy,
    /// Applying native proxy intent and state.
    ApplySystemProxy,
    /// Reconciling native proxy intent.
    ReconcileSystemProxy,
    /// Releasing owned native proxy state.
    ReleaseSystemProxy,
    /// Recovering service ownership into ordinary Local ownership.
    ServiceLocalRecovery,
    /// Restoring capture after service maintenance.
    RestoreMaintenanceProxy,
    /// Completing admitted service TUN activation.
    EnableServiceTun,
}

/// Capture failure retaining its original operation or task cause.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TrafficCaptureError {
    /// No active profile exists for a TUN configuration transition.
    #[error("{}", zenclash_i18n::text("traffic_capture.errors.missing_profile"))]
    MissingProfile,
    /// Capture would overwrite a proxy owned by another application.
    #[error(
        "{}",
        zenclash_i18n::text("traffic_capture.errors.external_system_proxy")
    )]
    ExternalSystemProxy,
    /// No usable HTTP/Mixed port is available for proxy activation or restoration.
    #[error(
        "{}",
        zenclash_i18n::text("traffic_capture.errors.missing_system_proxy_port")
    )]
    MissingSystemProxyPort,
    /// No persistent system-proxy session is available.
    #[error(
        "{}",
        zenclash_i18n::text("traffic_capture.errors.system_proxy_unavailable")
    )]
    SystemProxyUnavailable,
    /// Offline recovery cannot produce live capture observations.
    #[error("{}", zenclash_i18n::text("core_page.errors.offline_capture"))]
    OfflineRecovery,
    /// Observations do not confirm the state required for restoration.
    #[error("{}", zenclash_i18n::text("core_page.service.capture_restore_unknown"))]
    ObservationUnconfirmed,
    /// The service did not confirm runtime recovery.
    #[error(
        "{}",
        zenclash_i18n::text("core_page.service.handover_recovery_failed")
    )]
    RuntimeRecoveryUnconfirmed,
    /// The completed capture operation returned an explicit reconciliation requirement.
    /// Its detail is a display summary, not a reconstructed native error or recovery authority.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "traffic_capture.errors.reconciliation_required",
            &[("detail", detail.clone())]
        )
    )]
    ReconciliationRequired {
        /// Existing capture outcome's diagnostic summary.
        detail: String,
    },
    /// A legacy read-only observer supplied a display failure rather than an operation cause.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "traffic_capture.errors.observation_unavailable",
            &[("error", .0.clone())]
        )
    )]
    ObservationUnavailable(String),
    /// Native proxy validation, mutation, readback or persistence failed.
    #[error(transparent)]
    SystemProxy(#[from] SystemProxySessionError),
    /// Effective configuration or core transition failed.
    #[error(transparent)]
    Core(Box<CoreSessionError>),
    /// A controller request failed.
    #[error(transparent)]
    Controller(#[from] MihomoError),
    /// The enabled override chain could not be loaded.
    #[error(transparent)]
    Overrides(#[from] YamlOverrideError),
    /// An admitted task was cancelled or panicked.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "traffic_capture.errors.task",
            &[
                ("task", task.to_string()),
                ("error", source.to_string())
            ]
        )
    )]
    Task {
        /// Failed background task.
        task: TrafficCaptureTask,
        /// Original task failure.
        #[source]
        source: tokio::task::JoinError,
    },
    /// Runtime application and subsequent native proxy restoration both failed.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "traffic_capture.errors.rollback",
            &[
                ("cause", cause.to_string()),
                ("rollback", rollback.to_string())
            ]
        )
    )]
    Rollback {
        /// Original runtime failure.
        #[source]
        cause: Box<CoreSessionError>,
        /// Subsequent proxy restoration failure.
        rollback: Box<TrafficCaptureError>,
    },
}

impl From<CoreSessionError> for TrafficCaptureError {
    fn from(error: CoreSessionError) -> Self {
        match error {
            CoreSessionError::Capture(error) => *error,
            error => Self::Core(Box::new(error)),
        }
    }
}

impl From<TrafficCaptureError> for CoreSessionError {
    fn from(error: TrafficCaptureError) -> Self {
        Self::Capture(Box::new(error))
    }
}

impl TrafficCaptureError {
    pub(super) fn is_native_recovery_unconfirmed(&self) -> bool {
        let Self::SystemProxy(error) = self else {
            return false;
        };
        let native = match error {
            SystemProxySessionError::Native(MihomoError::SystemProxy(error)) => error.as_ref(),
            SystemProxySessionError::Transaction(error) => error.as_ref(),
            _ => return false,
        };
        matches!(
            native,
            crate::SystemProxyError::RecoveryPending
                | crate::SystemProxyError::RecoveryUnconfirmed { .. }
        )
    }
}

impl std::fmt::Display for TrafficCaptureTask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::Completion => "traffic_capture.tasks.completion",
            Self::LoadOverrides => "traffic_capture.tasks.load_overrides",
            Self::ReadSystemProxy => "traffic_capture.tasks.read_system_proxy",
            Self::ApplySystemProxy => "traffic_capture.tasks.apply_system_proxy",
            Self::ReconcileSystemProxy => "traffic_capture.tasks.reconcile_system_proxy",
            Self::ReleaseSystemProxy => "traffic_capture.tasks.release_system_proxy",
            Self::ServiceLocalRecovery => "traffic_capture.tasks.service_local_recovery",
            Self::RestoreMaintenanceProxy => "traffic_capture.tasks.restore_maintenance_proxy",
            Self::EnableServiceTun => "traffic_capture.tasks.enable_service_tun",
        }))
    }
}

pub(super) fn task_failure(
    task: TrafficCaptureTask,
    source: tokio::task::JoinError,
) -> TrafficCaptureError {
    TrafficCaptureError::Task { task, source }
}
