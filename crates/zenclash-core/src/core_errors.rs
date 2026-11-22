//! Shared domain error, version, and validation types for `zenclash-core`.
//!
//! Keeping these mutually related error enums in a leaf module prevents
//! bidirectional dependency cycles between transport, endpoints, preferences,
//! and system-proxy subsystems.

use std::{io, process::ExitStatus};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::platform_command::PlatformCommandError;

/// Result type returned by Mihomo process, endpoint and API operations.
pub type MihomoResult<T> = Result<T, MihomoError>;

/// Definitive native rejection of startup ownership, without exposing IPC internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStartupRejection {
    /// Another authenticated application already owns the service session.
    Occupied,
    /// The actual user is not approved to use the installation.
    Unauthorized,
    /// The verified service uses an incompatible protocol.
    Incompatible,
    /// Administrator maintenance must finish before ownership can be acquired.
    MaintenancePending,
}

/// Error produced while configuring, launching or communicating with Mihomo.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MihomoError {
    /// Controller URL is empty, malformed or uses an unsupported scheme.
    #[error("invalid Mihomo controller endpoint: {0}")]
    InvalidEndpoint(String),
    /// HTTP transport, timeout or response-decoding failure.
    #[error("Mihomo request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Local controller transport failure, retaining whether a request was sent.
    #[error("Mihomo IPC request failed: {0}")]
    Native(#[from] crate::service::NativeHttpError),
    /// Authenticated native service transport or policy rejection.
    #[error("Mihomo service request failed: {0}")]
    Service(#[from] crate::service::ServiceCallError),
    /// An admitted native mutation has no confirmed outcome yet.
    #[error("service runtime outcome is unconfirmed")]
    RuntimeOutcomeUnknown,
    /// Files or a core already changed before a later operation failed.
    #[error("service runtime changed before confirmation: {0}")]
    RuntimePartiallyApplied(#[source] Box<MihomoError>),
    /// A response belongs to a backend replaced before the request completed.
    #[error("Mihomo controller changed during the request")]
    StaleTransport,
    /// Binding changed before dispatch: no request was sent and the outcome is definitive.
    #[error("{}", zenclash_i18n::text("core_page.errors.binding_changed"))]
    StaleBinding,
    /// A Local Mihomo mutation requires authorization and service handover before dispatch.
    #[error("{}", zenclash_i18n::text("core_page.service.required"))]
    ServiceRequired,
    /// The controller's JSON response does not match the requested schema.
    #[error("invalid Mihomo response: {0}")]
    Decode(#[from] serde_json::Error),
    /// Response decoding failure annotated with its endpoint and JSON field path.
    #[error("invalid Mihomo response from {endpoint} at {field_path}: {source}")]
    ResponseDecode {
        /// Controller route without query parameters or authentication information.
        endpoint: String,
        /// Nested JSON field or array entry that could not be decoded.
        field_path: String,
        /// Original JSON decoding failure, retaining line and column information.
        #[source]
        source: serde_json::Error,
        /// Whether the original transport classified decoding failure as uncertain.
        outcome_unknown: bool,
    },
    /// A direct controller response exceeded its defensive byte limit.
    #[error("Mihomo response from {endpoint} exceeds {limit_bytes} bytes")]
    ResponseTooLarge {
        /// Controller route without query parameters or credentials.
        endpoint: String,
        /// Maximum accepted response size in bytes.
        limit_bytes: usize,
    },
    /// Non-success response returned by Mihomo, including its bounded message.
    #[error("Mihomo API returned HTTP {status}: {message}")]
    Api {
        /// HTTP response status code.
        status: u16,
        /// Error message returned by Mihomo.
        message: String,
    },
    /// Invalid caller input rejected before a network request is sent.
    #[error("invalid Mihomo request input: {0}")]
    InvalidInput(String),
    /// Process, filesystem or native-platform operation failure.
    #[error("Mihomo process error: {0}")]
    Process(String),
    /// System-proxy or PAC operation failure, preserving native and recovery facts.
    #[error(transparent)]
    SystemProxy(#[from] Box<SystemProxyError>),
}

impl From<std::io::Error> for MihomoError {
    fn from(error: std::io::Error) -> Self {
        Self::Process(error.to_string())
    }
}

impl MihomoError {
    /// Returns a definitive native startup rejection, if one was observed.
    /// Transport loss and timeouts remain unknown; this reader performs no IPC.
    #[must_use]
    pub fn service_startup_rejection(&self) -> Option<ServiceStartupRejection> {
        use crate::service::ServiceCallError;
        use zenclash_service::ServiceErrorCode;
        match self {
            Self::Service(ServiceCallError::Rejected { code, .. })
                if *code == ServiceErrorCode::UnauthorizedOwner as u16 =>
            {
                Some(ServiceStartupRejection::Unauthorized)
            }
            Self::Service(ServiceCallError::Rejected { code, .. })
                if *code == ServiceErrorCode::ProtocolMismatch as u16 =>
            {
                Some(ServiceStartupRejection::Incompatible)
            }
            Self::Service(ServiceCallError::VersionMismatch(_)) => {
                Some(ServiceStartupRejection::Incompatible)
            }
            _ => None,
        }
    }

    /// Reports whether a failed mutation may already have changed the controller.
    /// Policy/input rejection and failure to establish native IPC are definitive.
    #[must_use]
    pub fn mutation_result_unknown(&self) -> bool {
        match self {
            Self::Http(_)
            | Self::ResponseTooLarge { .. }
            | Self::StaleTransport
            | Self::RuntimeOutcomeUnknown
            | Self::RuntimePartiallyApplied(_) => true,
            Self::Service(error) => error.mutation_result_unknown(),
            Self::ResponseDecode {
                outcome_unknown, ..
            } => *outcome_unknown,
            Self::Native(crate::service::NativeHttpError::OutcomeUnknown(_)) => true,
            Self::Native(crate::service::NativeHttpError::BudgetExceeded { request_sent }) => {
                *request_sent
            }
            _ => false,
        }
    }
}

/// Response returned by Mihomo's `/version` endpoint.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct VersionInfo {
    /// Whether the running core identifies itself as Mihomo/Clash Meta.
    #[serde(default)]
    pub meta: bool,
    /// Core version string.
    #[serde(default)]
    pub version: String,
}

/// Errors produced while loading or saving application preferences.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AppPreferencesError {
    /// Filesystem access failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("preferences.errors.io", &[("error", .0.to_string())])
    )]
    Io(#[from] std::io::Error),
    /// The preferences document is malformed or incompatible.
    #[error(
        "{}",
        zenclash_i18n::text_with("preferences.errors.json", &[("error", .0.to_string())])
    )]
    Json(#[from] serde_json::Error),
    /// The document exceeded the defensive read limit.
    #[error("{}", zenclash_i18n::text("preferences.errors.too_large"))]
    TooLarge,
    /// A platform data directory could not be determined.
    #[error("{}", zenclash_i18n::text("preferences.errors.missing_data_directory"))]
    MissingDataDirectory,
    /// A preference value is outside its supported range.
    #[error(
        "{}",
        zenclash_i18n::text_with("preferences.errors.invalid", &[("error", .0.clone())])
    )]
    Invalid(String),
    /// Preferences changed after a reversible update was prepared.
    #[error(
        "{}",
        zenclash_i18n::text("preferences.errors.concurrent_modification")
    )]
    ConcurrentModification,
}

/// Result type for persistent application-preference operations.
pub type AppPreferencesResult<T> = Result<T, AppPreferencesError>;

/// Operation that failed while starting a PAC listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PacServerOperation {
    /// Binding the HTTP listener.
    Bind,
    /// Configuring the listener for nonblocking access.
    ConfigureListener,
    /// Reading the listener's local address.
    ReadLocalAddress,
    /// Spawning the HTTP worker.
    SpawnWorker,
}

/// PAC listener or worker startup failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PacError {
    /// A PAC startup operation failed, retaining the OS cause.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.pac.errors.io",
            &[
                ("operation", operation.to_string()),
                ("error", source.to_string())
            ]
        )
    )]
    Io {
        /// Failed listener or worker operation.
        operation: PacServerOperation,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
}

impl std::fmt::Display for PacServerOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::Bind => "system_proxy.pac.operations.bind",
            Self::ConfigureListener => "system_proxy.pac.operations.configure_listener",
            Self::ReadLocalAddress => "system_proxy.pac.operations.read_local_address",
            Self::SpawnWorker => "system_proxy.pac.operations.spawn_worker",
        }))
    }
}

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

pub(crate) fn invalid_setting(
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
