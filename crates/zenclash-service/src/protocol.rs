use std::fmt;

use serde::{Deserialize, Serialize};

/// Wire protocol version; independent from the installation metadata schema.
pub const PROTOCOL_VERSION: u32 = 4;

/// Service handshake information, containing no client-supplied identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolInfo {
    /// Wire protocol supported by the service.
    pub protocol_version: u32,
    /// Installed service build version.
    pub service_version: String,
}

impl ProtocolInfo {
    /// Describes the current build.
    pub fn current() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            service_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Reports whether this build understands the service's protocol.
    pub fn is_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }
}

/// Random session credential, redacted from diagnostics.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionToken(pub(crate) [u8; 32]);

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// An opaque credential bound to the verified session owner and generation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionProof {
    generation: u64,
    token: SessionToken,
}

impl SessionProof {
    #[cfg(any(feature = "server", test))]
    pub(crate) fn new(generation: u64, token: SessionToken) -> Self {
        Self { generation, token }
    }

    /// Monotonic session generation chosen by the service.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(feature = "server")]
    pub(crate) fn authenticates(&self, other: &Self) -> bool {
        let mut difference = 0_u8;
        for (left, right) in self.token.0.iter().zip(other.token.0.iter()) {
            difference |= left ^ right;
        }
        difference == 0 && self.generation == other.generation
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Hello {
        protocol_version: u32,
    },
    Acquire {},
    Inspect {},
    Session {
        proof: SessionProof,
        sequence: u64,
        operation: SessionOperation,
    },
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hello { protocol_version } => f
                .debug_struct("Hello")
                .field("protocol_version", protocol_version)
                .finish(),
            Self::Acquire {} => f.write_str("Acquire"),
            Self::Inspect {} => f.write_str("Inspect"),
            Self::Session {
                proof,
                sequence,
                operation,
            } => f
                .debug_struct("Session")
                .field("proof", proof)
                .field("sequence", sequence)
                .field("operation", operation)
                .finish(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SessionOperation {
    Release {},
    Status {},
    Stage {
        config: String,
    },
    UploadAsset {
        path: String,
        offset: u64,
        bytes: Vec<u8>,
        finished: bool,
    },
    Start {
        revision: u64,
    },
    Validate {
        revision: u64,
    },
    Reload {
        revision: u64,
        force: bool,
    },
    CommitRuntime {
        revision: u64,
    },
    PrepareRuntimePatch {
        base_revision: u64,
        patch: serde_json::Value,
    },
    ApplyRuntimePatch {
        revision: u64,
    },
    RestoreRuntimePatch {
        revision: u64,
    },
    Stop {},
    BeginProviderCacheRead {
        revision: u64,
        kind: ProviderKind,
        name: String,
    },
    ReadProviderCache {
        token: ProviderCacheToken,
        offset: u64,
    },
    FinishProviderCacheRead {
        token: ProviderCacheToken,
    },
    Logs {
        cursor: u64,
    },
    Api {
        request: ApiRequest,
    },
    Subscribe {
        stream: StreamKind,
    },
    SubscribeLogs {
        options: LogStreamOptions,
    },
}

impl fmt::Debug for SessionOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release {} => f.write_str("Release"),
            Self::Status {} => f.write_str("Status"),
            Self::Stage { .. } => f.write_str("Stage { config: [redacted] }"),
            Self::UploadAsset {
                offset, finished, ..
            } => f
                .debug_struct("UploadAsset")
                .field("path", &"[redacted]")
                .field("offset", offset)
                .field("bytes", &"[redacted]")
                .field("finished", finished)
                .finish(),
            Self::Start { revision } => {
                f.debug_struct("Start").field("revision", revision).finish()
            }
            Self::Validate { revision } => f
                .debug_struct("Validate")
                .field("revision", revision)
                .finish(),
            Self::Reload { revision, force } => f
                .debug_struct("Reload")
                .field("revision", revision)
                .field("force", force)
                .finish(),
            Self::CommitRuntime { revision } => f
                .debug_struct("CommitRuntime")
                .field("revision", revision)
                .finish(),
            Self::PrepareRuntimePatch { base_revision, .. } => f
                .debug_struct("PrepareRuntimePatch")
                .field("base_revision", base_revision)
                .field("patch", &"[redacted]")
                .finish(),
            Self::ApplyRuntimePatch { revision } => f
                .debug_struct("ApplyRuntimePatch")
                .field("revision", revision)
                .finish(),
            Self::RestoreRuntimePatch { revision } => f
                .debug_struct("RestoreRuntimePatch")
                .field("revision", revision)
                .finish(),
            Self::Stop {} => f.write_str("Stop"),
            Self::BeginProviderCacheRead { revision, kind, .. } => f
                .debug_struct("BeginProviderCacheRead")
                .field("revision", revision)
                .field("kind", kind)
                .field("name", &"[redacted]")
                .finish(),
            Self::ReadProviderCache { offset, .. } => f
                .debug_struct("ReadProviderCache")
                .field("offset", offset)
                .field("token", &"[redacted]")
                .finish(),
            Self::FinishProviderCacheRead { .. } => f.write_str("FinishProviderCacheRead"),
            Self::Logs { cursor } => f.debug_struct("Logs").field("cursor", cursor).finish(),
            Self::Api { request } => f.debug_struct("Api").field("request", request).finish(),
            Self::Subscribe { stream } => {
                f.debug_struct("Subscribe").field("stream", stream).finish()
            }
            Self::SubscribeLogs { options } => f
                .debug_struct("SubscribeLogs")
                .field("options", options)
                .finish(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApiRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) body: Option<serde_json::Value>,
}

impl fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiRequest")
            .field("method", &self.method)
            .field("path", &"[redacted]")
            .field("body", &"[redacted]")
            .finish()
    }
}

/// Namespace of a declared HTTP provider cache.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Proxy provider YAML; readback validates its approved resource references.
    Proxy,
    /// Rule provider content, including opaque MRS bytes.
    Rule,
}

/// Opaque readback credential usable only within its verified owner session.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderCacheToken(pub(crate) [u8; 32]);

impl fmt::Debug for ProviderCacheToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Outcome of reading a declared cache from a confirmed stopped runtime.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderCacheRead {
    /// The declared cache is absent; no snapshot token was reserved.
    Absent,
    /// An immutable bounded snapshot is ready for sequential paging.
    Ready {
        /// Credential of the sole retained snapshot.
        token: ProviderCacheToken,
        /// Number of bytes after safe proxy resource-path normalization.
        len: u64,
        /// SHA-256 of the exact returned bytes, independent of mutable cache metadata.
        sha256: [u8; 32],
    },
}

/// One sequential page of an immutable provider snapshot.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCacheChunk {
    /// Byte offset supplied by the caller.
    pub offset: u64,
    /// At most 256 KiB of cache bytes.
    pub bytes: Vec<u8>,
    /// Whether this page reached the end of the snapshot.
    pub finished: bool,
}

impl fmt::Debug for ProviderCacheChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderCacheChunk")
            .field("offset", &self.offset)
            .field("bytes", &"[redacted]")
            .field("finished", &self.finished)
            .finish()
    }
}

/// Response from a service-approved kernel API request.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiResponse {
    /// HTTP status returned by the restricted kernel API.
    pub status: u16,
    /// JSON response body forwarded by the service.
    pub body: serde_json::Value,
}

/// A partial revision and the safe fields its application will actually send.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRuntimePatch {
    /// Revision of the retained partial candidate.
    pub revision: u64,
    /// Validated effective delta, including observed TUN enable when necessary.
    pub effective_patch: serde_json::Value,
}

impl fmt::Debug for PreparedRuntimePatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedRuntimePatch")
            .field("revision", &self.revision)
            .field("effective_patch", &"[redacted]")
            .finish()
    }
}

impl fmt::Debug for ApiResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiResponse")
            .field("status", &self.status)
            .field("body", &"[redacted]")
            .finish()
    }
}

/// Severity threshold for a named service log subscription.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLogLevel {
    /// Disable log events.
    Silent,
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warning,
    /// Operational events, warnings and errors.
    #[default]
    Info,
    /// Include verbose diagnostic events.
    Debug,
}

/// JSON representation requested from the kernel log stream.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLogFormat {
    /// Legacy type/payload JSON events.
    Plain,
    /// Events with time, level, message and structured fields.
    #[default]
    Structured,
}

/// Explicit bounded log options; defaults in Rust are Info and Structured.
///
/// Every wire request must supply both fields; missing or unknown fields fail.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogStreamOptions {
    /// Severity threshold requested from the kernel.
    pub level: ServiceLogLevel,
    /// Kernel event representation to request.
    pub format: ServiceLogFormat,
}

/// Named kernel event streams available through the service policy.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// Kernel log entries.
    Logs,
    /// Aggregate upload and download rates.
    Traffic,
    /// Current kernel connections.
    Connections,
    /// Kernel memory observations.
    Memory,
}

/// Latest service observation of the managed kernel process.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStatus {
    /// Single retained candidate and its last confirmed operation phase.
    pub candidate: Option<RuntimeCandidate>,
    /// Revision currently applied by the kernel, including an uncommitted candidate.
    pub applied_revision: Option<u64>,
    /// Last revision committed by the application's configuration transaction.
    pub committed_revision: Option<u64>,
    /// Whether the service currently owns a live kernel process.
    pub running: bool,
    /// Verified service-managed kernel process identifier.
    pub pid: Option<u32>,
    /// Last observed process exit reason, when available.
    pub exit_reason: Option<String>,
}

/// Bounded observation of the session's uncommitted runtime candidate.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCandidate {
    /// Candidate's service-assigned revision.
    pub revision: u64,
    /// Accepted revision used by a partial candidate; absent for complete configurations.
    pub base_revision: Option<u64>,
    /// Whether the candidate replaces a full configuration or only managed fields.
    pub kind: RuntimeCandidateKind,
    /// Last phase proven by the service, never inferred from a failed connection.
    pub phase: RuntimeCandidatePhase,
}

/// The configuration operation represented by a runtime candidate.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCandidateKind {
    /// Complete staged configuration and resource snapshot.
    Full,
    /// Bounded patch sharing the accepted revision's resource roots.
    Patch,
}

/// Last confirmed candidate phase, independent from application business persistence.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCandidatePhase {
    /// Configuration is prepared; its changes are not currently applied.
    Prepared,
    /// Complete configuration passed approved-kernel validation.
    Validated,
    /// Changes were verified in the same managed kernel.
    Applied,
    /// A mutation may have applied and cannot yet be confirmed by readback.
    Uncertain,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Response {
    Inspection {
        busy: bool,
    },
    Hello {
        info: ProtocolInfo,
    },
    Acquired {
        proof: SessionProof,
        next_sequence: u64,
    },
    Ok,
    Status {
        snapshot: RuntimeStatus,
    },
    Staged {
        revision: u64,
    },
    RuntimePatchPrepared {
        prepared: PreparedRuntimePatch,
    },
    ProviderCacheRead {
        snapshot: ProviderCacheRead,
    },
    ProviderCacheChunk {
        chunk: ProviderCacheChunk,
    },
    Logs {
        cursor: u64,
        lines: Vec<String>,
    },
    Api {
        response: ApiResponse,
    },
    Stream {
        data: serde_json::Value,
    },
    Error {
        code: ServiceErrorCode,
    },
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection { busy } => f.debug_struct("Inspection").field("busy", busy).finish(),
            Self::Hello { info } => f.debug_struct("Hello").field("info", info).finish(),
            Self::Acquired {
                proof,
                next_sequence,
            } => f
                .debug_struct("Acquired")
                .field("proof", proof)
                .field("next_sequence", next_sequence)
                .finish(),
            Self::Ok => f.write_str("Ok"),
            Self::Status { snapshot } => f
                .debug_struct("Status")
                .field("snapshot", snapshot)
                .finish(),
            Self::Staged { revision } => f
                .debug_struct("Staged")
                .field("revision", revision)
                .finish(),
            Self::RuntimePatchPrepared { prepared } => f
                .debug_struct("RuntimePatchPrepared")
                .field("prepared", prepared)
                .finish(),
            Self::ProviderCacheRead { snapshot } => f
                .debug_struct("ProviderCacheRead")
                .field("snapshot", snapshot)
                .finish(),
            Self::ProviderCacheChunk { chunk } => f
                .debug_struct("ProviderCacheChunk")
                .field("chunk", chunk)
                .finish(),
            Self::Logs { cursor, .. } => f
                .debug_struct("Logs")
                .field("cursor", cursor)
                .field("lines", &"[redacted]")
                .finish(),
            Self::Api { response } => f.debug_struct("Api").field("response", response).finish(),
            Self::Stream { .. } => f.write_str("Stream { data: [redacted] }"),
            Self::Error { code } => f.debug_struct("Error").field("code", code).finish(),
        }
    }
}

/// Structured service rejection reasons for application-level presentation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceErrorCode {
    /// An administrator installation transaction must finish or recover first.
    MaintenancePending,
    /// Client and service protocols differ.
    Incompatible,
    /// Native identity or session credentials were rejected.
    Unauthorized,
    /// Another application process owns the runtime.
    Occupied,
    /// The session lease elapsed.
    Expired,
    /// The request sequence has already been admitted.
    Replay,
    /// The request is unsupported or malformed.
    InvalidRequest,
    /// Runtime configuration violates the service policy.
    InvalidConfiguration,
    /// Asset name or chunk violates the staging policy.
    InvalidAsset,
    /// The operation exceeds a bounded resource budget.
    BudgetExceeded,
    /// No configuration has been staged.
    NotStaged,
    /// The requested runtime revision is no longer current.
    StaleRevision,
    /// The approved kernel is unavailable or invalid.
    KernelUnavailable,
    /// The kernel failed to execute the operation.
    KernelFailed,
    /// A mutation was sent but its final kernel result could not be confirmed.
    OutcomeUnknown,
    /// The service could not complete an internal operation.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_log_wire_options_require_both_fields_and_reject_query_injection() {
        for options in [
            serde_json::json!({}),
            serde_json::json!({"level":"info"}),
            serde_json::json!({"format":"structured"}),
            serde_json::json!({"level":"warn", "format":"plain"}),
            serde_json::json!({"level":"trace", "format":"plain"}),
            serde_json::json!({"level":"info", "format":"unsupported"}),
            serde_json::json!({"level":"info", "format":"structured", "path":"/configs"}),
        ] {
            assert!(
                serde_json::from_value::<SessionOperation>(serde_json::json!({
                    "action":"subscribe_logs", "options":options
                }))
                .is_err()
            );
        }
        let operation: SessionOperation = serde_json::from_value(serde_json::json!({
            "action":"subscribe_logs", "options":{"level":"warning", "format":"plain"}
        }))
        .unwrap();
        assert!(matches!(
            operation,
            SessionOperation::SubscribeLogs {
                options: LogStreamOptions {
                    level: ServiceLogLevel::Warning,
                    format: ServiceLogFormat::Plain
                }
            }
        ));
    }

    #[test]
    fn wire_roundtrip_keeps_staged_content_but_diagnostics_redact_it() {
        let request = Request::Session {
            proof: SessionProof::new(1, SessionToken([0xab; 32])),
            sequence: 1,
            operation: SessionOperation::Stage {
                config: "secret: controller-private-token".into(),
            },
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        let decoded: Request = serde_json::from_slice(&bytes).unwrap();
        assert!(
            matches!(decoded, Request::Session { operation: SessionOperation::Stage { config }, .. } if config == "secret: controller-private-token")
        );
        assert!(!format!("{request:?}").contains("controller-private-token"));
    }

    #[test]
    fn client_supplied_os_identity_and_unknown_operations_are_rejected() {
        for message in [
            serde_json::json!({"type":"acquire", "uid":0}),
            serde_json::json!({"type":"execute", "command":"sh"}),
            serde_json::json!({"action":"start", "revision":1, "executable":"/bin/sh"}),
            serde_json::json!({"action":"stop", "pid":1}),
            serde_json::json!({"action":"status", "uid":0}),
            serde_json::json!({"action":"release", "generation":99}),
        ] {
            let rejected = if message.get("type").is_some() {
                serde_json::from_value::<Request>(message).is_err()
            } else {
                serde_json::from_value::<SessionOperation>(message).is_err()
            };
            assert!(rejected);
        }
    }

    #[test]
    fn incompatible_protocols_cannot_be_treated_as_ready() {
        let current = ProtocolInfo::current();
        assert!(current.is_compatible());
        let incompatible: ProtocolInfo = serde_json::from_value(serde_json::json!({
            "protocol_version": PROTOCOL_VERSION + 1,
            "service_version": "0.1.2"
        }))
        .unwrap();
        assert!(!incompatible.is_compatible());
    }

    #[test]
    fn atomic_maintenance_requires_protocol_three_and_rejects_legacy_helpers() {
        assert_eq!(ProtocolInfo::current().protocol_version, 4);
        for version in [1, 2] {
            let legacy = ProtocolInfo {
                protocol_version: version,
                service_version: "0.1.2".into(),
            };
            assert!(!legacy.is_compatible());
        }
    }

    #[test]
    fn diagnostic_formatting_does_not_expose_session_secrets() {
        let proof = SessionProof::new(7, SessionToken([0xab; 32]));
        let diagnostic = format!("{proof:?}");
        assert!(diagnostic.contains("[redacted]"));
        assert!(!diagnostic.contains("171"));
        assert!(!diagnostic.contains("abab"));
    }

    #[test]
    fn malformed_session_credentials_are_rejected_at_the_wire_boundary() {
        assert!(
            serde_json::from_value::<SessionProof>(serde_json::json!({
                "generation": 1,
                "token": [1, 2, 3]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProtocolInfo>(serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "service_version": "0.1.2",
                "owner_uid": 0
            }))
            .is_err()
        );
    }
}
