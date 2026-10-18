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
    #[error("{}", zenclash_i18n::text("system_proxy.validation.empty"))]
    Empty,
    /// A value exceeds its byte budget.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.validation.too_long",
            &[("maximum", maximum.to_string())]
        )
    )]
    TooLong {
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// Too many bypass entries were supplied.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.validation.too_many",
            &[("maximum", maximum.to_string())]
        )
    )]
    TooMany {
        /// Maximum accepted entries.
        maximum: usize,
    },
    /// The value has invalid syntax.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.invalid_syntax"))]
    InvalidSyntax,
    /// A wildcard listener address is not a valid proxy endpoint.
    #[error(
        "{}",
        zenclash_i18n::text("system_proxy.validation.unspecified_address")
    )]
    UnspecifiedAddress,
    /// Port zero cannot identify a proxy endpoint.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.zero_port"))]
    ZeroPort,
    /// A persisted claim is not in canonical form.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.not_canonical"))]
    NotCanonical,
    /// The claimed network service is missing, oversized or not canonical.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.invalid_service"))]
    InvalidService,
    /// URL parsing failed.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.validation.invalid_pac_url",
            &[("error", .0.to_string())]
        )
    )]
    InvalidPacUrl(#[source] url::ParseError),
    /// PAC requires an HTTP URL without credentials.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.unsafe_pac_url"))]
    UnsafePacUrl,
    /// The PAC document contains a NUL byte.
    #[error("{}", zenclash_i18n::text("system_proxy.validation.embedded_nul"))]
    EmbeddedNul,
    /// The PAC entry point is missing.
    #[error(
        "{}",
        zenclash_i18n::text("system_proxy.validation.missing_pac_function")
    )]
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
    /// Command writes cannot safely replace and recover an authenticated proxy.
    AuthenticatedProxy,
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
#[error("{}", self.display_message())]
pub struct NativePropertyListError {
    operation: &'static str,
    domain: Option<String>,
    code: i64,
}

impl NativePropertyListError {
    fn display_message(&self) -> String {
        zenclash_i18n::text_with(
            "system_proxy.errors.property_list",
            &[
                ("operation", self.operation.to_owned()),
                ("code", self.code.to_string()),
                ("domain", format!("{:?}", self.domain)),
            ],
        )
    }

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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.validation",
            &[("field", field.to_string()), ("reason", reason.to_string())]
        )
    )]
    Validation {
        /// Rejected setting category.
        field: SystemProxySetting,
        /// Validation reason without raw input.
        #[source]
        reason: SystemProxyValidationError,
    },
    /// A native function returned a failure code.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.platform",
            &[
                ("backend", backend.to_string()),
                ("operation", operation.to_string()),
                ("code", code.to_string())
            ]
        )
    )]
    Platform {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Native function name without preference values.
        operation: &'static str,
        /// Original OS status code.
        code: i64,
    },
    /// A native operation produced an I/O error.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.platform_io",
            &[
                ("backend", backend.to_string()),
                ("operation", operation.to_string()),
                ("error", source.to_string())
            ]
        )
    )]
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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.native_operation_rejected",
            &[
                ("backend", backend.to_string()),
                ("operation", operation.to_string())
            ]
        )
    )]
    NativeOperationRejected {
        /// Native preference backend.
        backend: SystemProxyBackendKind,
        /// Failed native operation.
        operation: &'static str,
    },
    /// A required native capability is unavailable.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.backend_unavailable",
            &[("backend", backend.to_string()), ("reason", reason.to_string())]
        )
    )]
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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.discovery_exit",
            &[("status", status.to_string())]
        )
    )]
    DiscoveryExit {
        /// Original exit status, without potentially sensitive captured output.
        status: ExitStatus,
    },
    /// A networksetup write could not be executed. Arguments are omitted.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.network_setup_command",
            &[("operation", operation.to_string()), ("error", source.to_string())]
        )
    )]
    NetworkSetupCommand {
        /// Fixed command option, without user-supplied values.
        operation: &'static str,
        /// Typed process failure.
        #[source]
        source: PlatformCommandError,
    },
    /// networksetup rejected a write or returned diagnostic output.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.errors.network_setup_rejected",
            &[("operation", operation.to_string()), ("status", status.to_string())]
        )
    )]
    NetworkSetupRejected {
        /// Fixed command option, without user-supplied values.
        operation: &'static str,
        /// Exit status; captured output can contain private values and is omitted.
        status: ExitStatus,
    },
    /// PAC listener creation or worker startup failed.
    #[error(transparent)]
    Pac(#[from] PacError),
    /// An earlier operation still retains recovery resources.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.pending_recovery"))]
    RecoveryPending,
    /// Current native state does not grant permission for a conditional write.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.ownership_changed"))]
    OwnershipChanged,
    /// A native capture changed while being read and cannot establish a stable snapshot.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.observation_unstable"))]
    ObservationUnstable,
    /// Native input could not be decoded or exceeded the snapshot budget.
    #[error("{}", zenclash_i18n::text("system_proxy.errors.invalid_snapshot"))]
    InvalidNativeSnapshot {
        /// Original property-list error, when supplied by the native decoder.
        #[source]
        cause: Option<NativePropertyListError>,
    },
    /// Native preferences could not be serialized into a retained snapshot.
    #[error(
        "{}",
        zenclash_i18n::text("system_proxy.errors.snapshot_encoding_failed")
    )]
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
    #[error("{}", zenclash_i18n::text_with("system_proxy.errors.recovery_failure", &[("error", cause.to_string()), ("recovery", recovery.to_string()), ("release", release_failure.as_ref().map_or_else(|| zenclash_i18n::text("system_proxy.errors.no_release_failure"), ToString::to_string))]))]
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
    #[error("{}", zenclash_i18n::text("system_proxy.errors.unsupported_platform"))]
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
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::Port => "system_proxy.settings.port",
            Self::Host => "system_proxy.settings.host",
            Self::Bypass => "system_proxy.settings.bypass",
            Self::PacScript => "system_proxy.settings.pac_script",
            Self::PacUrl => "system_proxy.settings.pac_url",
            Self::Ownership => "system_proxy.settings.ownership",
        }))
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
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::NoUsableService => "system_proxy.reasons.no_usable_service",
            Self::AmbiguousService => "system_proxy.reasons.ambiguous_service",
            Self::NoPersistentSettings => "system_proxy.reasons.no_persistent_settings",
            Self::NoSchemas => "system_proxy.reasons.no_schemas",
            Self::MissingSchema => "system_proxy.reasons.missing_schema",
            Self::LockedSetting => "system_proxy.reasons.locked_setting",
            Self::UnverifiedStorageContract => "system_proxy.reasons.unverified_storage_contract",
            Self::AuthenticatedProxy => "system_proxy.reasons.authenticated_proxy",
        }))
    }
}

#[cfg(test)]
mod tests;
