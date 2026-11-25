// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
//! Request authentication, owner lifecycle gating, owner transitions, and JSON response helpers.

use crate::core::auth::{
    AuthenticatedOwner, ServiceError, authenticate_owner, hash_session_token, validate_request_origin_and_host,
};
use crate::core::desired::{
    ActiveOwnerState, clear_active_owner, commit_active_owner_session, persist_owner_core_started,
    persist_owner_core_stopped, persist_owner_core_stopped_by_key,
};
use crate::core::manager::{CORE_IPC_READY_TIMEOUT, CORE_MANAGER, CORE_OUTPUT_DRAIN_TIMEOUT};
use crate::core::runtime_generation::PreparedRuntime;
use crate::core::state::set_core_lifecycle_state;
use crate::core::structure::{OwnerSessionProof, Response, ServiceLifecycleState};
use crate::{
    AuthenticatedRequest, AuthenticatedSessionRequest, MIN_SUPPORTED_CLIENT_REVISION, ProtocolVersion,
    SERVICE_PROTOCOL_HEADER,
};
use anyhow::{Context as _, Result as AnyResult, anyhow};
use http::StatusCode;
use kode_bridge::{Result, ipc_http_server::HttpResponse};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    ops::ControlFlow,
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, MutexGuard};

pub(super) static ADMISSION_CLOSED: AtomicBool = AtomicBool::new(false);
pub(super) static OWNER_LIFECYCLE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Handler time a takeover needs after stopping the previous core.
const TAKEOVER_RESERVE: Duration = CORE_IPC_READY_TIMEOUT.saturating_add(Duration::from_secs(4));
/// Handler time a stop needs after the core has exited.
pub(super) const STOP_RESERVE: Duration = CORE_OUTPUT_DRAIN_TIMEOUT.saturating_add(Duration::from_secs(1));

pub(super) trait OwnerTransition {
    async fn stop_previous_core(&mut self) -> AnyResult<()>;
    async fn start_new_core(&mut self) -> AnyResult<()>;
    async fn commit_new_owner(&mut self) -> AnyResult<ActiveOwnerState>;
}

pub(super) async fn run_owner_transition(
    transition: &mut impl OwnerTransition,
) -> std::result::Result<ActiveOwnerState, ServiceError> {
    if let Err(stop_error) = transition.stop_previous_core().await {
        return Err(ServiceError::owner_switch_failed(format!(
            "Failed to stop the previous owner core: {stop_error:#}"
        )));
    }
    transition
        .start_new_core()
        .await
        .map_err(|error| ServiceError::owner_switch_failed(format!("Failed to start owner core: {error:#}")))?;
    transition
        .commit_new_owner()
        .await
        .map_err(|error| ServiceError::owner_switch_failed(format!("Failed to commit owner state: {error:#}")))
}

/// When a handler's stop must reach SIGKILL so `reserve` still fits before the handler timeout.
pub(super) fn sigkill_deadline(handler_deadline: Instant, reserve: Duration) -> Option<Instant> {
    Some(handler_deadline.checked_sub(reserve).unwrap_or_else(Instant::now))
}

pub(super) struct StartOwnerTransition<'a> {
    pub handler_deadline: Instant,
    pub previous_owner: Option<ActiveOwnerState>,
    pub owner: &'a AuthenticatedOwner,
    pub prepared_runtime: Option<PreparedRuntime>,
    pub proposed_session_token: &'a str,
}

impl OwnerTransition for StartOwnerTransition<'_> {
    async fn stop_previous_core(&mut self) -> AnyResult<()> {
        let kill_by = sigkill_deadline(self.handler_deadline, TAKEOVER_RESERVE);
        CORE_MANAGER.lock().await.stop_core_by(kill_by).await?;
        if let Some(previous_owner) = self.previous_owner.as_ref() {
            persist_owner_core_stopped_by_key(&previous_owner.owner_key)
                .await
                .context("failed to persist the previous owner stopped state")?;
        }
        clear_active_owner()
            .await
            .context("failed to clear the previous active owner")?;
        Ok(())
    }

    async fn start_new_core(&mut self) -> AnyResult<()> {
        let prepared = self
            .prepared_runtime
            .as_ref()
            .context("prepared runtime is unavailable")?;
        let clash_config = prepared.clash_config().clone();
        // Planning is read-only; materialize only after the previous core has stopped.
        prepared
            .materialize()
            .await
            .context("failed to materialize the runtime generation")?;
        let core_manager = CORE_MANAGER.lock().await;
        let start_result = core_manager.start_core(clash_config, self.owner.identity.clone()).await;
        drop(core_manager);
        if let Err(error) = start_result {
            let kill_by = sigkill_deadline(self.handler_deadline, STOP_RESERVE);
            if let Err(stop_error) = CORE_MANAGER.lock().await.stop_core_by(kill_by).await {
                return Err(anyhow!(
                    "{error:#}; failed to confirm termination of the rejected core: {stop_error:#}"
                ));
            }
            let _ = persist_owner_core_stopped(self.owner).await;
            // Keep the owner's durable generation and core-managed state for a retry.
            return Err(error);
        }
        Ok(())
    }

    async fn commit_new_owner(&mut self) -> AnyResult<ActiveOwnerState> {
        let clash_config = self
            .prepared_runtime
            .as_ref()
            .context("prepared runtime is unavailable during owner commit")?
            .clash_config();
        if let Err(error) = persist_owner_core_started(self.owner, clash_config).await {
            return self.rollback_commit_failure(error).await;
        }
        match commit_active_owner_session(self.owner, self.proposed_session_token).await {
            Ok(active) => {
                self.prepared_runtime
                    .take()
                    .context("prepared runtime disappeared during owner commit")?
                    .commit();
                crate::core::owner_lease::reset(Some(active.generation));
                Ok(active)
            }
            Err(error) => self.rollback_commit_failure(error).await,
        }
    }
}

impl StartOwnerTransition<'_> {
    async fn rollback_commit_failure<T>(&mut self, error: anyhow::Error) -> AnyResult<T> {
        if let Err(rollback_error) = rollback_started_owner(self.owner, self.handler_deadline).await {
            return Err(anyhow!(
                "{error:#}; failed to roll back uncommitted owner core: {rollback_error:#}"
            ));
        }
        // The generation is durable, so rolling back ownership has no disk changes to undo.
        self.prepared_runtime.take();
        Err(error)
    }
}

async fn rollback_started_owner(owner: &AuthenticatedOwner, handler_deadline: Instant) -> AnyResult<()> {
    let kill_by = sigkill_deadline(handler_deadline, STOP_RESERVE);
    if let Err(stop_error) = CORE_MANAGER.lock().await.stop_core_by(kill_by).await {
        set_core_lifecycle_state(ServiceLifecycleState::Fatal);
        return Err(anyhow!(
            "failed to terminate owner core during rollback: {stop_error:#}"
        ));
    }

    let desired_result = persist_owner_core_stopped(owner).await;
    let active_result = clear_active_owner().await;
    match (desired_result, active_result) {
        (Ok(_), Ok(())) => Ok(()),
        (Err(desired_error), Ok(())) => Err(desired_error),
        (Ok(_), Err(active_error)) => Err(active_error),
        (Err(desired_error), Err(active_error)) => {
            set_core_lifecycle_state(ServiceLifecycleState::Fatal);
            Err(anyhow!(
                "failed to persist stopped owner state: {desired_error:#}; failed to clear active owner: {active_error:#}"
            ))
        }
    }
}

pub(super) fn require_protocol_version(ctx: &kode_bridge::RequestContext) -> std::result::Result<(), ServiceError> {
    let supplied = ctx
        .headers
        .get(SERVICE_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok());
    let Some(supplied) = supplied.and_then(ProtocolVersion::parse_header) else {
        return Err(ServiceError::protocol_mismatch());
    };
    let current = ProtocolVersion::current();
    (supplied.epoch == current.epoch && supplied.revision >= MIN_SUPPORTED_CLIENT_REVISION)
        .then_some(())
        .ok_or_else(ServiceError::protocol_mismatch)
}

/// Common credential access for owner-only and session-authenticated envelopes.
pub(super) trait OwnerRequestEnvelope {
    fn credentials(&self) -> &crate::OwnerCredentials;
}

impl<T> OwnerRequestEnvelope for AuthenticatedRequest<T> {
    fn credentials(&self) -> &crate::OwnerCredentials {
        &self.credentials
    }
}

impl<T> OwnerRequestEnvelope for AuthenticatedSessionRequest<T> {
    fn credentials(&self) -> &crate::OwnerCredentials {
        &self.credentials
    }
}

/// Validates Host/Origin, protocol version, parses the envelope, then authenticates its owner.
/// Lifecycle locking remains separate so `StartClash` can validate before waiting on the lock.
pub(super) fn authenticate_request<E>(
    ctx: &kode_bridge::RequestContext,
) -> ControlFlow<Result<HttpResponse>, (E, AuthenticatedOwner)>
where
    E: DeserializeOwned + OwnerRequestEnvelope,
{
    if let Err(error) = validate_request_origin_and_host(&ctx.headers) {
        return ControlFlow::Break(service_error(error));
    }
    if let Err(error) = require_protocol_version(ctx) {
        return ControlFlow::Break(service_error(error));
    }
    let request = match ctx.json::<E>() {
        Ok(request) => request,
        Err(error) => return ControlFlow::Break(bad_request(format!("Invalid JSON: {error}"))),
    };
    let owner = match authenticate_owner(ctx, request.credentials()) {
        Ok(owner) => owner,
        Err(error) => return ControlFlow::Break(service_error(error)),
    };
    ControlFlow::Continue((request, owner))
}

/// Authentication state required after acquiring the owner lifecycle lock.
pub(super) enum OwnerLifecycleGate<'a> {
    /// No active owner is required.
    Unchecked,
    /// The authenticated owner must be active.
    ActiveOwner,
    /// The request must prove the current active session.
    ActiveSession(&'a OwnerSessionProof),
}

/// Acquires `OWNER_LIFECYCLE_LOCK`, applies the gate, and returns the guard to the caller.
pub(super) async fn enter_owner_lifecycle(
    owner: &AuthenticatedOwner,
    gate: OwnerLifecycleGate<'_>,
) -> ControlFlow<Result<HttpResponse>, OwnerLifecycleGuard> {
    let lifecycle_guard = OWNER_LIFECYCLE_LOCK.lock().await;
    if ADMISSION_CLOSED.load(Ordering::Acquire) {
        return ControlFlow::Break(service_unavailable(
            "The idle service is exiting; reconnect to activate it",
        ));
    }
    let gated = match gate {
        OwnerLifecycleGate::Unchecked => Ok(()),
        OwnerLifecycleGate::ActiveOwner => require_active_owner(owner).await,
        OwnerLifecycleGate::ActiveSession(proof) => require_active_session(owner, proof).await.map(|_| ()),
    };
    match gated {
        Ok(()) => ControlFlow::Continue(OwnerLifecycleGuard {
            _guard: lifecycle_guard,
            generation: match gate {
                OwnerLifecycleGate::ActiveSession(proof) => Some(proof.generation),
                _ => None,
            },
        }),
        Err(error) => ControlFlow::Break(service_error(error)),
    }
}

pub(super) struct OwnerLifecycleGuard {
    _guard: MutexGuard<'static, ()>,
    generation: Option<u64>,
}

impl Drop for OwnerLifecycleGuard {
    fn drop(&mut self) {
        if let Some(generation) = self.generation {
            crate::core::owner_lease::renew(generation);
        }
    }
}

pub(super) async fn read_log_snapshot(path: &std::path::Path) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

    // Stay below kode-bridge's 10 MiB in-memory response limit after hex encoding.
    const MAX_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    if length > MAX_SNAPSHOT_BYTES {
        file.seek(std::io::SeekFrom::Start(length - MAX_SNAPSHOT_BYTES)).await?;
    }
    let mut content = Vec::with_capacity(length.min(MAX_SNAPSHOT_BYTES) as usize);
    file.read_to_end(&mut content).await?;
    if length > MAX_SNAPSHOT_BYTES
        && let Some(first_newline) = content.iter().position(|byte| *byte == b'\n')
    {
        content.drain(..=first_newline);
    }
    let mut encoded = String::with_capacity(content.len() * 2);
    for byte in content {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(encoded)
}

pub(super) fn ok_json<T: Serialize>(data: T) -> Result<HttpResponse> {
    json_response(StatusCode::OK, 0, "Success", Some(data))
}

pub(super) fn ok_empty(message: impl Into<String>) -> Result<HttpResponse> {
    json_response::<()>(StatusCode::OK, 0, message, None)
}

pub(super) fn service_unavailable(message: impl Into<String>) -> Result<HttpResponse> {
    json_response::<()>(StatusCode::SERVICE_UNAVAILABLE, 1, message, None)
}

pub(super) fn bad_request(message: impl Into<String>) -> Result<HttpResponse> {
    json_response::<()>(StatusCode::BAD_REQUEST, StatusCode::BAD_REQUEST.as_u16(), message, None)
}

pub(super) fn service_error(error: ServiceError) -> Result<HttpResponse> {
    let status = match error.code {
        crate::ServiceErrorCode::UnauthorizedOwner | crate::ServiceErrorCode::AppDataRootNotOwned => {
            StatusCode::UNAUTHORIZED
        }
        crate::ServiceErrorCode::ForbiddenOrigin => StatusCode::FORBIDDEN,
        crate::ServiceErrorCode::NotActive | crate::ServiceErrorCode::SessionConflict => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    json_response::<()>(status, error.code as u16, error.message, None)
}

pub(super) async fn require_active_owner(owner: &AuthenticatedOwner) -> std::result::Result<(), ServiceError> {
    if crate::core::desired::load_active_owner()
        .await
        .map_err(|_| ServiceError::not_active())?
        .is_some_and(|active| active.owner_key == owner.key)
    {
        Ok(())
    } else {
        Err(ServiceError::not_active())
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

pub(super) async fn require_active_session(
    owner: &AuthenticatedOwner,
    proof: &OwnerSessionProof,
) -> std::result::Result<ActiveOwnerState, ServiceError> {
    let active = crate::core::desired::load_active_owner()
        .await
        .map_err(|_| ServiceError::stale_owner_session())?
        .ok_or_else(ServiceError::stale_owner_session)?;
    let supplied_hash = hash_session_token(&proof.token).map_err(|_| ServiceError::stale_owner_session())?;
    if active.owner_key != owner.key
        || active.generation != proof.generation
        || !constant_time_eq(active.session_token_hash.as_bytes(), supplied_hash.as_bytes())
    {
        return Err(ServiceError::stale_owner_session());
    }
    Ok(active)
}

fn json_response<T: Serialize>(
    status: StatusCode,
    code: u16,
    message: impl Into<String>,
    data: Option<T>,
) -> Result<HttpResponse> {
    let json_value = Response {
        code,
        message: message.into(),
        data,
    };
    Ok(HttpResponse::builder().status(status).json(&json_value)?.build())
}
