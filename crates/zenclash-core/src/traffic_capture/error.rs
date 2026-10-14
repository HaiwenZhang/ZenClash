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
    #[error("没有可用于流量接管的活动 Profile")]
    MissingProfile,
    /// Capture would overwrite a proxy owned by another application.
    #[error("系统代理由其他应用控制；请先在系统设置中处理后再切换接管方式")]
    ExternalSystemProxy,
    /// No usable HTTP/Mixed port is available for proxy activation or restoration.
    #[error("没有可用的 HTTP/Mixed 端口")]
    MissingSystemProxyPort,
    /// No persistent system-proxy session is available.
    #[error("系统代理持久化状态不可用")]
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
    #[error("流量接管需要协调：{detail}")]
    ReconciliationRequired {
        /// Existing capture outcome's diagnostic summary.
        detail: String,
    },
    /// A legacy read-only observer supplied a display failure rather than an operation cause.
    #[error("流量接管观察失败：{0}")]
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
    #[error("流量接管后台任务异常结束（{task}）：{source}")]
    Task {
        /// Failed background task.
        task: TrafficCaptureTask,
        /// Original task failure.
        #[source]
        source: tokio::task::JoinError,
    },
    /// Runtime application and subsequent native proxy restoration both failed.
    #[error("运行操作失败：{cause}；系统代理回滚失败：{rollback}")]
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
        formatter.write_str(match self {
            Self::Completion => "完成接管事务",
            Self::LoadOverrides => "加载覆写链",
            Self::ReadSystemProxy => "读取系统代理",
            Self::ApplySystemProxy => "应用系统代理",
            Self::ReconcileSystemProxy => "恢复系统代理",
            Self::ReleaseSystemProxy => "释放系统代理",
            Self::ServiceLocalRecovery => "恢复本地内核",
            Self::RestoreMaintenanceProxy => "维护后恢复系统代理",
            Self::EnableServiceTun => "启用服务 TUN",
        })
    }
}

pub(super) fn task_failure(
    task: TrafficCaptureTask,
    source: tokio::task::JoinError,
) -> TrafficCaptureError {
    TrafficCaptureError::Task { task, source }
}
