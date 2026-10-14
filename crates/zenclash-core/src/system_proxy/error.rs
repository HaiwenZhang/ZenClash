//! System-proxy failure facts, separate from process failures and display summaries.

use super::pac::PacError;
use crate::{AppPreferencesError, MihomoError, PlatformCommandError};
use std::{io, process::ExitStatus};
use thiserror::Error;

/// The system-proxy setting rejected before native mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemProxySetting {
    /// Manual proxy port.
    Port,
    /// Manual proxy or PAC listener host.
    Host,
    /// Proxy bypass rules.
    Bypass,
    /// PAC script document.
    PacScript,
    /// PAC URL.
    PacUrl,
    /// Persisted managed-endpoint claim.
    Ownership,
}

/// Why a system-proxy setting was rejected, without retaining its sensitive input.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SystemProxyValidationError {
    /// A required value is empty.
    #[error("值不能为空")]
    Empty,
    /// A value exceeds its byte budget.
    #[error("超过 {maximum} 字节限制")]
    TooLong {
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// Too many bypass entries were supplied.
    #[error("最多支持 {maximum} 条")]
    TooMany {
        /// Maximum accepted entries.
        maximum: usize,
    },
    /// The value has invalid syntax.
    #[error("格式无效")]
    InvalidSyntax,
    /// A wildcard listener address is not a valid proxy endpoint.
    #[error("不能是未指定地址")]
    UnspecifiedAddress,
    /// Port zero cannot identify a proxy endpoint.
    #[error("端口不能为 0")]
    ZeroPort,
    /// A persisted claim is not in canonical form.
    #[error("所有权不是规范形式")]
    NotCanonical,
    /// The claimed network service is missing, oversized or not canonical.
    #[error("所有权服务名称无效")]
    InvalidService,
    /// URL parsing failed.
    #[error("PAC URL 无效：{0}")]
    InvalidPacUrl(#[source] url::ParseError),
    /// PAC requires an HTTP URL without credentials.
    #[error("PAC URL 必须是无凭据的 HTTP 地址")]
    UnsafePacUrl,
    /// The PAC document contains a NUL byte.
    #[error("PAC 脚本不能包含 NUL 字节")]
    EmbeddedNul,
    /// The PAC entry point is missing.
    #[error("PAC 脚本必须定义 FindProxyForURL")]
    MissingPacFunction,
}

/// Native preference backend involved in a system-proxy failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemProxyBackendKind {
    /// macOS System Configuration preferences.
    MacOs,
    /// Windows current-user WinINET preferences.
    WinInet,
    /// GNOME GSettings proxy preferences.
    Gnome,
}

/// A native backend prerequisite that is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemProxyBackendUnavailable {
    /// No usable network service was found.
    NoUsableService,
    /// The requested service cannot be identified uniquely.
    AmbiguousService,
    /// The backend cannot persist settings.
    NoPersistentSettings,
    /// The proxy schema source is unavailable.
    NoSchemas,
    /// A required proxy schema is missing.
    MissingSchema,
    /// A setting rejects user writes.
    LockedSetting,
    /// Native option and raw storage facts cannot support exact restoration.
    UnverifiedStorageContract,
}

/// The original failure that caused a native transaction to roll back.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SystemProxyTransactionCause {
    /// Native read, validation, apply or refresh failure.
    #[error(transparent)]
    Native(#[from] MihomoError),
    /// Preference persistence failed after the native operation.
    #[error(transparent)]
    Preferences(#[from] AppPreferencesError),
}

/// Immutable numeric facts copied from a property-list CFError before it is released.
/// Descriptions and user-info dictionaries are omitted because they can contain input data.
#[derive(Debug, Error)]
#[non_exhaustive]
#[error("CoreFoundation {operation} 失败：{code}（{domain:?}）")]
pub struct NativePropertyListError {
    operation: &'static str,
    domain: Option<String>,
    code: i64,
}

impl NativePropertyListError {
    #[cfg(target_os = "macos")]
    pub(crate) fn new(operation: &'static str, domain: Option<String>, code: i64) -> Self {
        Self {
            operation,
            domain,
            code,
        }
    }

    /// Returns the failed native property-list operation.
    #[must_use]
    pub fn operation(&self) -> &'static str {
        self.operation
    }

    /// Returns the original error domain, when it could be decoded.
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// Returns the original CFError code.
    #[must_use]
    pub fn code(&self) -> i64 {
        self.code
    }
}

/// Failure of system-proxy validation, native access, PAC or recovery.
/// Native preference values and PAC source documents are never stored in this error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SystemProxyError {
    /// Caller input was rejected before mutation.
    #[error("系统代理设置无效（{field}）：{reason}")]
    Validation {
        /// Rejected setting category.
        field: SystemProxySetting,
        /// Validation reason without raw input.
        #[source]
        reason: SystemProxyValidationError,
    },
    /// A native function returned a failure code.
    #[error("系统代理原生操作失败（{backend}/{operation}）：{code}")]
    Platform {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Native function name without preference values.
        operation: &'static str,
        /// Original OS status code.
        code: i64,
    },
    /// A native operation produced an I/O error.
    #[error("系统代理原生操作失败（{backend}/{operation}）：{source}")]
    PlatformIo {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Failed operation.
        operation: &'static str,
        /// Original OS error.
        #[source]
        source: io::Error,
    },
    /// A native function failed without providing an OS status code.
    #[error("系统代理原生操作失败（{backend}/{operation}），原生函数未提供错误码")]
    NativeOperationRejected {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Failed native operation.
        operation: &'static str,
    },
    /// A required native capability is unavailable.
    #[error("系统代理后端不可用（{backend}）：{reason}")]
    BackendUnavailable {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Missing prerequisite.
        reason: SystemProxyBackendUnavailable,
    },
    /// A read-only network-service discovery command failed to run.
    #[error(transparent)]
    DiscoveryCommand(#[from] PlatformCommandError),
    /// Discovery ran but returned a failing exit status.
    #[error("networksetup 退出状态：{status}")]
    DiscoveryExit {
        /// Original exit status, without potentially sensitive captured output.
        status: ExitStatus,
    },
    /// PAC listener creation or worker startup failed.
    #[error(transparent)]
    Pac(#[from] PacError),
    /// An earlier operation still retains recovery resources.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.pending_recovery"))]
    RecoveryPending,
    /// Current native state does not grant permission for a conditional write.
    #[error("系统代理原生状态已变化，拒绝覆盖")]
    OwnershipChanged,
    /// A native capture changed while being read and cannot establish a stable snapshot.
    #[error("系统代理原生状态在读取时发生变化")]
    ObservationUnstable,
    /// Native input could not be decoded or exceeded the snapshot budget.
    #[error("系统代理原生设置格式无效或超过快照限制")]
    InvalidNativeSnapshot {
        /// Original property-list error, when supplied by the native decoder.
        #[source]
        cause: Option<NativePropertyListError>,
    },
    /// Native preferences could not be serialized into a retained snapshot.
    #[error("无法编码系统代理原生快照")]
    SnapshotEncodingFailed {
        /// Original property-list error, when supplied by the native encoder.
        #[source]
        cause: Option<NativePropertyListError>,
    },
    /// Native readback differs from the state the adapter attempted.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.verification"))]
    VerificationFailed,
    /// The operation failed but the previous native state was restored and refreshed.
    #[error("{}", zenclash_i18n::text_with("system_proxy.errors.recovered_failure", &[("error", cause.to_string())]))]
    RecoveryConfirmed {
        /// Original failure before recovery.
        #[source]
        cause: Box<SystemProxyTransactionCause>,
    },
    /// Recovery could not be confirmed; retained resources still require recovery.
    #[error("{}", zenclash_i18n::text_with("system_proxy.errors.recovery_failure", &[("error", cause.to_string()), ("recovery", recovery.to_string()), ("release", release_failure.as_ref().map_or_else(|| "未报告额外关闭错误".into(), ToString::to_string))]))]
    RecoveryUnconfirmed {
        /// Original failure before recovery.
        #[source]
        cause: Box<SystemProxyTransactionCause>,
        /// Recovery failure, retaining its original type and source.
        recovery: Box<MihomoError>,
        /// Additional safety-disable failure; absence does not prove recovery.
        release_failure: Option<Box<MihomoError>>,
    },
    /// The platform has no native system-proxy adapter.
    #[error("当前平台尚未实现系统代理控制")]
    UnsupportedPlatform,
}

impl From<SystemProxyError> for MihomoError {
    fn from(error: SystemProxyError) -> Self {
        Self::SystemProxy(Box::new(error))
    }
}

pub(super) fn invalid_setting(
    field: SystemProxySetting,
    reason: SystemProxyValidationError,
) -> MihomoError {
    SystemProxyError::Validation { field, reason }.into()
}

impl std::fmt::Display for SystemProxySetting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Port => "端口",
            Self::Host => "主机",
            Self::Bypass => "绕过规则",
            Self::PacScript => "PAC 脚本",
            Self::PacUrl => "PAC URL",
            Self::Ownership => "所有权",
        })
    }
}

impl std::fmt::Display for SystemProxyBackendKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MacOs => "macOS",
            Self::WinInet => "WinINET",
            Self::Gnome => "GNOME",
        })
    }
}

impl std::fmt::Display for SystemProxyBackendUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoUsableService => "未找到可用网络服务",
            Self::AmbiguousService => "无法唯一定位网络服务",
            Self::NoPersistentSettings => "持久设置后端不可用",
            Self::NoSchemas => "代理 schema 来源不可用",
            Self::MissingSchema => "缺少代理 schema",
            Self::LockedSetting => "代理设置禁止写入",
            Self::UnverifiedStorageContract => "原生代理存储表示无法验证",
        })
    }
}

#[cfg(test)]
mod tests;
