// Forked from Clash Verge Rev core/service.rs, adapted 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json for copied symbols and changes.

use anyhow::{Context as _, Result};
use parking_lot::RwLock;
use std::path::Path;
use tokio::sync::Mutex;
use zenclash_service::{
    MacosProxyConfig, OwnerCredentials, OwnerSessionProof, ProtocolInfo, ProxyApplyOutcome,
    RuntimeBundle, RuntimeFileOutcome, RuntimeFileRequest, ServiceErrorCode, ServiceStatusSnapshot,
    StageRuntimeOutcome, StartClashRequest, StartClashResult, WriterConfig,
};

/// Capabilities of the service session that owns the running Core.
/// They are discarded with that session rather than cached across service upgrades.
#[derive(Clone)]
struct ActiveServiceSession {
    proof: OwnerSessionProof,
    supports_runtime_staging: bool,
    supports_runtime_file_read: bool,
}

fn generate_service_session_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).context("failed to generate service owner session")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone, Copy, Default)]
struct ServiceCapabilities {
    runtime_staging: bool,
    runtime_file_read: bool,
}

impl ServiceCapabilities {
    const fn of(info: &ProtocolInfo) -> Self {
        Self {
            runtime_staging: info.supports_runtime_staging(),
            runtime_file_read: info.supports_runtime_file_read(),
        }
    }
}

fn session_matches_status(
    proof: &OwnerSessionProof,
    is_active: bool,
    active_generation: Option<u64>,
) -> bool {
    is_active && active_generation == Some(proof.generation)
}

/// Typed service refusals remain distinguishable from an unanswered IPC mutation.
#[derive(Debug, thiserror::Error)]
pub enum ServiceCallError {
    #[error("service rejected request ({code}): {message}")]
    /// The service returned a definite refusal with a protocol error code.
    Rejected {
        /// Stable protocol error code returned by the service.
        code: u16,
        /// Service diagnostic explaining the refusal.
        message: String,
    },
    #[error("service owner session is not active")]
    /// No acknowledged owner session is available for this operation.
    NoActiveSession,
    /// A proposed token is retained until native Start or authenticated Stop is acknowledged.
    #[error("service start outcome is unconfirmed")]
    StartUnconfirmed,
    #[error("service owner generation changed")]
    /// A newer owner generation displaced this session.
    OwnerLost,
    #[error("owned core process changed while controller request was running")]
    /// The owned core PID changed during a controller operation.
    ControllerChanged,
    #[error("service helper version or protocol mismatch: {0}")]
    /// The installed helper cannot satisfy the required version or protocol.
    VersionMismatch(String),
    #[error("service does not support {0}")]
    /// The active helper lacks the named protocol capability.
    UnsupportedCapability(&'static str),
    #[error("service did not return {0}")]
    /// A successful reply omitted the named required payload.
    MissingReply(&'static str),
    #[error(transparent)]
    /// The native Mihomo control channel failed.
    Controller(#[from] crate::service::NativeHttpError),
    #[error(transparent)]
    /// The service control channel failed.
    Transport(#[from] anyhow::Error),
}

impl ServiceCallError {
    /// A failed mutation may have taken effect before its response or owner check failed.
    /// Native HTTP failures retain whether any request bytes could have been sent.
    pub fn mutation_result_unknown(&self) -> bool {
        match self {
            Self::StartUnconfirmed
            | Self::Transport(_)
            | Self::MissingReply(_)
            | Self::ControllerChanged
            | Self::OwnerLost => true,
            Self::Controller(crate::service::NativeHttpError::OutcomeUnknown(_)) => true,
            Self::Controller(crate::service::NativeHttpError::BudgetExceeded { request_sent }) => {
                *request_sent
            }
            Self::Rejected { code, .. } => !matches!(*code,
                value if value == ServiceErrorCode::UnauthorizedOwner as u16
                    || value == ServiceErrorCode::NotActive as u16
                    || value == ServiceErrorCode::InvalidInstallLocation as u16
                    || value == ServiceErrorCode::InvalidRuntimeAsset as u16
                    || value == ServiceErrorCode::ProtocolMismatch as u16
                    || value == ServiceErrorCode::StaleOwnerSession as u16
                    || value == ServiceErrorCode::InvalidProxyConfig as u16
                    || value == ServiceErrorCode::AppDataRootNotOwned as u16),
            _ => false,
        }
    }
}

/// Owns the native proof for one application session, shared by its UI/controller handles.
/// It does not start a core when constructed; start requires a complete runtime bundle.
pub struct ServiceSession {
    credentials: OwnerCredentials,
    active: RwLock<Option<ActiveServiceSession>>,
    pending_start: RwLock<Option<String>>,
    observed_status: RwLock<Option<ServiceStatusSnapshot>>,
    mutations: Mutex<()>,
}

impl std::fmt::Debug for ServiceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceSession")
            .field("owner", &self.credentials.identity)
            .field("has_session", &self.active.read().is_some())
            .finish_non_exhaustive()
    }
}

impl ServiceSession {
    /// Verifies the native service and loads upstream owner credentials for this app root.
    ///
    /// # Errors
    /// Returns native identity/token errors, transport errors, service refusals or incompatible protocol information.
    pub async fn connect(app_root: &Path) -> Result<Self, ServiceCallError> {
        let credentials = crate::service::owner_credentials(app_root)?;
        let response = zenclash_service::get_version().await?;
        check_response(response.code, response.message)?;
        let info = response
            .data
            .ok_or(ServiceCallError::MissingReply("protocol information"))?;
        if info.build_version != zenclash_service::VERSION
            || !info.supports_client(
                zenclash_service::ProtocolVersion::current(),
                zenclash_service::MIN_REQUIRED_SERVICE_REVISION,
            )
        {
            return Err(ServiceCallError::VersionMismatch(info.build_version));
        }
        Ok(Self {
            credentials,
            active: RwLock::new(None),
            pending_start: RwLock::new(None),
            observed_status: RwLock::new(None),
            mutations: Mutex::new(()),
        })
    }

    /// Starts with a new proposed token; an unanswered mutation retains its shutdown authority.
    ///
    /// # Errors
    /// Returns validation or native refusal errors. A lost/cancelled response leaves a retained proposed token and an unconfirmed outcome.
    pub async fn start(
        &self,
        runtime: RuntimeBundle,
    ) -> Result<StartClashResult, ServiceCallError> {
        self.start_exchange(runtime, |request| async move {
            let response = zenclash_service::start_clash(&self.credentials, &request).await?;
            check_response(response.code, response.message)?;
            response
                .data
                .ok_or(ServiceCallError::MissingReply("owner session information"))
        })
        .await
    }

    async fn start_exchange<F, Fut>(
        &self,
        runtime: RuntimeBundle,
        exchange: F,
    ) -> Result<StartClashResult, ServiceCallError>
    where
        F: FnOnce(StartClashRequest) -> Fut,
        Fut: std::future::Future<Output = Result<StartClashResult, ServiceCallError>>,
    {
        let _mutation = self.mutations.lock().await;
        if self.pending_start.read().is_some() {
            return Err(ServiceCallError::StartUnconfirmed);
        }
        self.observed_status.write().take();
        let proposed_session_token = generate_service_session_token()?;
        let request = StartClashRequest {
            runtime,
            proposed_session_token: proposed_session_token.clone(),
            macos_proxy: None,
        };
        // Save before dispatch, so cancellation cannot abandon a possibly started core.
        // The previous acknowledged proof stays available for a definitive Start refusal.
        *self.pending_start.write() = Some(proposed_session_token.clone());
        let result = match exchange(request).await {
            Ok(result) => result,
            Err(error) => {
                if !error.mutation_result_unknown() {
                    self.pending_start.write().take();
                }
                return Err(error);
            }
        };
        // Commit authority before any further await, including optional capability discovery.
        *self.active.write() = Some(ActiveServiceSession {
            proof: OwnerSessionProof {
                generation: result.session.generation,
                token: proposed_session_token,
            },
            supports_runtime_staging: false,
            supports_runtime_file_read: false,
        });
        self.pending_start.write().take();
        let capabilities = probe_service_capabilities().await;
        if let Some(session) = self.active.write().as_mut() {
            session.supports_runtime_staging = capabilities.runtime_staging;
            session.supports_runtime_file_read = capabilities.runtime_file_read;
        }
        Ok(result)
    }

    /// Captures the exact proof required by protected service mutations.
    ///
    /// # Errors
    /// Returns `StartUnconfirmed` for a pending Start or `NoActiveSession` without an acknowledged owner proof.
    pub fn active_proof(&self) -> Result<OwnerSessionProof, ServiceCallError> {
        if self.pending_start.read().is_some() {
            return Err(ServiceCallError::StartUnconfirmed);
        }
        self.active
            .read()
            .as_ref()
            .map(|session| session.proof.clone())
            .ok_or(ServiceCallError::NoActiveSession)
    }

    /// Returns the owner-scoped Mihomo control socket/pipe chosen by the service.
    pub fn core_ipc_path(&self) -> String {
        zenclash_service::mihomo_ipc_path(&self.credentials.identity)
    }

    /// Calls Mihomo directly over its native controller, using an authenticated owner PID.
    /// The secret comes from the caller's immutable runtime snapshot. This is not a service RPC relay.
    ///
    /// # Errors
    /// Returns lost ownership, native peer, transport or budget errors. Requests sent before a failure may have taken effect.
    pub async fn controller_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        secret: &str,
        timeout: std::time::Duration,
    ) -> Result<crate::service::NativeHttpResponse, ServiceCallError> {
        let (proof, pid, controller) = self.controller_for_owner(secret).await?;
        let response = controller.request(method, path, body, timeout).await?;
        self.confirm_controller_owner(&proof, pid).await?;
        Ok(response)
    }

    /// Opens a bounded Mihomo native WebSocket; callers retire it with the controller binding.
    ///
    /// # Errors
    /// Returns lost ownership, native peer, connection or WebSocket handshake errors.
    pub async fn controller_socket(
        &self,
        path: &str,
        secret: &str,
    ) -> Result<crate::service::NativeSocket, ServiceCallError> {
        let (proof, pid, controller) = self.controller_for_owner(secret).await?;
        let socket = controller.websocket(path).await?;
        self.confirm_controller_owner(&proof, pid).await?;
        Ok(socket)
    }

    async fn controller_for_owner(
        &self,
        secret: &str,
    ) -> Result<
        (
            OwnerSessionProof,
            u32,
            crate::service::controller::NativeController,
        ),
        ServiceCallError,
    > {
        let proof = self.active_proof()?;
        let status = self.status().await?;
        if self.active_proof().ok().as_ref() != Some(&proof)
            || !session_matches_status(&proof, status.is_active, status.active_generation)
        {
            return Err(ServiceCallError::OwnerLost);
        }
        let pid = status
            .core_pid
            .ok_or(ServiceCallError::MissingReply("owned core PID"))?;
        Ok((
            proof,
            pid,
            crate::service::controller::NativeController::new(
                self.core_ipc_path().into(),
                pid,
                secret.to_owned(),
            ),
        ))
    }

    async fn confirm_controller_owner(
        &self,
        proof: &OwnerSessionProof,
        pid: u32,
    ) -> Result<(), ServiceCallError> {
        let status = self.status().await?;
        if self.active_proof().ok().as_ref() != Some(proof)
            || !session_matches_status(proof, status.is_active, status.active_generation)
        {
            return Err(ServiceCallError::OwnerLost);
        }
        if status.core_pid != Some(pid) {
            return Err(ServiceCallError::ControllerChanged);
        }
        Ok(())
    }

    /// Reads authenticated owner state without replacing a lost proof with a new generation.
    ///
    /// # Errors
    /// Returns native transport or service-response errors; failed observations invalidate the cached status.
    pub async fn status(&self) -> Result<ServiceStatusSnapshot, ServiceCallError> {
        let result = async {
            let response = zenclash_service::get_status(&self.credentials).await?;
            check_response(response.code, response.message)?;
            response
                .data
                .ok_or(ServiceCallError::MissingReply("owner status"))
        }
        .await;
        *self.observed_status.write() = result.as_ref().ok().cloned();
        result
    }

    /// Reads a previous authenticated observation for the current generation without I/O.
    /// Failure to refresh or replacement of the proof invalidates this prepared observation.
    pub fn snapshot(&self) -> Option<ServiceStatusSnapshot> {
        let observed = self.observed_status.read().clone()?;
        self.owns_status(&observed).then_some(observed)
    }

    /// Applies the copied generation check to an owner monitor observation.
    pub fn owns_status(&self, status: &ServiceStatusSnapshot) -> bool {
        if self.pending_start.read().is_some() {
            return false;
        }
        self.active.read().as_ref().is_some_and(|session| {
            session_matches_status(&session.proof, status.is_active, status.active_generation)
        })
    }

    /// Samples the captured generation using upstream owner-watch decisions.
    /// A concurrent local start/stop invalidates an older sample instead of displacing the new proof.
    pub async fn owner_sample(&self) -> crate::service::runstate::OwnerSample {
        use crate::service::runstate::OwnerSample;
        let proof = match self.active_proof() {
            Ok(proof) => proof,
            Err(ServiceCallError::StartUnconfirmed) => return OwnerSample::Unreadable,
            Err(_) => return OwnerSample::NotActive,
        };
        let status = self.status().await;
        if self.active_proof().ok().as_ref() != Some(&proof) {
            return OwnerSample::Unreadable;
        }
        match status {
            Ok(status) => OwnerSample::Status {
                is_active: session_matches_status(
                    &proof,
                    status.is_active,
                    status.active_generation,
                ),
                desired_core_should_be_running: status.desired_core_should_be_running,
                service_state: status.service_state,
                core_pid: status.core_pid,
            },
            Err(ServiceCallError::Rejected { code, .. })
                if code == ServiceErrorCode::NotActive as u16 =>
            {
                OwnerSample::NotActive
            }
            Err(_) => OwnerSample::Unreadable,
        }
    }

    /// Stages files in place; callers load the returned config path through Mihomo's controller.
    ///
    /// # Errors
    /// Returns missing proof, unsupported capability, owner or transport errors; staging may have partially changed native files.
    pub async fn stage_runtime(
        &self,
        runtime: &RuntimeBundle,
    ) -> Result<StageRuntimeOutcome, ServiceCallError> {
        let _mutation = self.mutations.lock().await;
        self.active_proof()?;
        let session = self
            .active
            .read()
            .clone()
            .ok_or(ServiceCallError::NoActiveSession)?;
        if !session.supports_runtime_staging {
            return Err(ServiceCallError::UnsupportedCapability("runtime staging"));
        }
        let response =
            zenclash_service::stage_runtime(&self.credentials, &session.proof, runtime).await?;
        check_response(response.code, response.message)?;
        response
            .data
            .ok_or(ServiceCallError::MissingReply("runtime staging outcome"))
    }

    /// Stops only the captured owner session; a stale proof cannot stop a replacement session.
    ///
    /// # Errors
    /// Returns native owner, transport or unconfirmed-Start errors. Only an authenticated native acknowledgement retires the held proof.
    pub async fn stop(&self) -> Result<(), ServiceCallError> {
        let _mutation = self.mutations.lock().await;
        let pending_token = self.pending_start.read().clone();
        if let Some(token) = pending_token {
            let status = self.status().await?;
            let generation = status
                .active_generation
                .filter(|_| status.is_active)
                .ok_or(ServiceCallError::StartUnconfirmed)?;
            // Status supplies no session token. Use our retained proposed token solely
            // for Stop: the native service verifies both token and generation. Never
            // adopt a controller or stop a replacement merely because its SID matches.
            let proof = OwnerSessionProof { generation, token };
            let response = zenclash_service::stop_clash(&self.credentials, &proof).await?;
            check_response(response.code, response.message)?;
            self.pending_start.write().take();
            self.active.write().take();
            self.observed_status.write().take();
            return Ok(());
        }
        let Some(session) = self.active.read().clone() else {
            return Ok(());
        };
        let response = zenclash_service::stop_clash(&self.credentials, &session.proof).await?;
        if response.code > 0
            && response.code != ServiceErrorCode::NotActive as u16
            && response.code != ServiceErrorCode::StaleOwnerSession as u16
        {
            return Err(ServiceCallError::Rejected {
                code: response.code,
                message: response.message,
            });
        }
        self.active.write().take();
        self.observed_status.write().take();
        Ok(())
    }

    /// Reads a manifest-declared provider cache chunk using the captured proof.
    ///
    /// # Errors
    /// Returns missing proof, unsupported capability, native owner or transport errors.
    pub async fn read_runtime_file(
        &self,
        request: &RuntimeFileRequest,
    ) -> Result<RuntimeFileOutcome, ServiceCallError> {
        self.active_proof()?;
        let session = self
            .active
            .read()
            .clone()
            .ok_or(ServiceCallError::NoActiveSession)?;
        if !session.supports_runtime_file_read {
            return Err(ServiceCallError::UnsupportedCapability("runtime file read"));
        }
        let response =
            zenclash_service::read_runtime_file(&self.credentials, &session.proof, request).await?;
        check_response(response.code, response.message)?;
        response
            .data
            .ok_or(ServiceCallError::MissingReply("runtime file outcome"))
    }

    /// Updates service log rotation for the current owner session.
    ///
    /// # Errors
    /// Returns missing proof, native owner, service refusal or transport errors; an unanswered mutation may have applied.
    pub async fn update_writer(&self, writer: &WriterConfig) -> Result<(), ServiceCallError> {
        let _mutation = self.mutations.lock().await;
        let proof = self.active_proof()?;
        let response = zenclash_service::update_writer(&self.credentials, &proof, writer).await?;
        check_response(response.code, response.message)
    }

    /// Applies macOS proxy settings through the same protected native session as upstream.
    ///
    /// # Errors
    /// Returns missing proof, native owner, invalid proxy or transport errors; proxy mutations may have partial effects.
    pub async fn set_system_proxy(
        &self,
        proxy: &MacosProxyConfig,
    ) -> Result<ProxyApplyOutcome, ServiceCallError> {
        let _mutation = self.mutations.lock().await;
        let proof = self.active_proof()?;
        let response = zenclash_service::set_system_proxy(&self.credentials, &proof, proxy).await?;
        check_response(response.code, response.message)?;
        response
            .data
            .ok_or(ServiceCallError::MissingReply("proxy outcome"))
    }

    /// Reads the current owner's buffered service core logs.
    ///
    /// # Errors
    /// Returns native transport or service-response errors; no log data is synthesized.
    pub async fn log_snapshot(&self) -> Result<String, ServiceCallError> {
        let response = zenclash_service::get_clash_log_snapshot(&self.credentials).await?;
        check_response(response.code, response.message)?;
        response
            .data
            .ok_or(ServiceCallError::MissingReply("core logs"))
    }
}

fn check_response(code: u16, message: String) -> Result<(), ServiceCallError> {
    if code == 0 {
        Ok(())
    } else {
        Err(ServiceCallError::Rejected { code, message })
    }
}

/// Failed capability probes select the upstream restart path instead of blocking start.
async fn probe_service_capabilities() -> ServiceCapabilities {
    match zenclash_service::get_version().await {
        Ok(response) if response.code == 0 => response
            .data
            .as_ref()
            .map_or_else(ServiceCapabilities::default, ServiceCapabilities::of),
        Ok(response) => {
            tracing::warn!(
                code = response.code,
                message = response.message,
                "service capability query was refused"
            );
            ServiceCapabilities::default()
        }
        Err(error) => {
            tracing::warn!(%error, "service capability query failed; configuration changes require restart");
            ServiceCapabilities::default()
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::service::NativeHttpError;

    #[test]
    fn controller_failure_keeps_the_send_boundary() {
        for (error, expected) in [
            (
                NativeHttpError::BeforeSend(std::io::Error::other("peer mismatch")),
                false,
            ),
            (
                NativeHttpError::OutcomeUnknown(std::io::Error::other("response lost")),
                true,
            ),
            (
                NativeHttpError::BudgetExceeded {
                    request_sent: false,
                },
                false,
            ),
            (NativeHttpError::BudgetExceeded { request_sent: true }, true),
        ] {
            assert_eq!(
                ServiceCallError::Controller(error).mutation_result_unknown(),
                expected
            );
        }
    }

    #[test]
    fn protocol_and_input_refusals_are_definitive_but_partial_failures_are_unknown() {
        for (code, unknown) in [
            (ServiceErrorCode::UnauthorizedOwner, false),
            (ServiceErrorCode::InvalidRuntimeAsset, false),
            (ServiceErrorCode::ProtocolMismatch, false),
            (ServiceErrorCode::StaleOwnerSession, false),
            (ServiceErrorCode::OwnerSwitchFailed, true),
            (ServiceErrorCode::ProxyClearFailed, true),
            (ServiceErrorCode::ProxyApplyFailed, true),
        ] {
            assert_eq!(
                ServiceCallError::Rejected {
                    code: code as u16,
                    message: String::new()
                }
                .mutation_result_unknown(),
                unknown
            );
        }
        assert!(ServiceCallError::OwnerLost.mutation_result_unknown());
    }
}

#[cfg(all(test, feature = "service-ipc-tests"))]
mod native_pending_tests;
