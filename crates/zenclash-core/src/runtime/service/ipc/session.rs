// Forked from Clash Verge Rev core/service.rs, adapted 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json for copied symbols and changes.

use anyhow::{Context as _, Result};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
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
    _keepalive: Arc<SessionKeepalive>,
}

struct SessionKeepalive(tokio::task::JoinHandle<()>);

enum RenewalProof {
    Acknowledged(OwnerSessionProof),
    Proposed(String),
}

impl SessionKeepalive {
    fn start(credentials: OwnerCredentials, proof: OwnerSessionProof) -> Self {
        Self::spawn(credentials, RenewalProof::Acknowledged(proof))
    }

    fn pending(credentials: OwnerCredentials, token: String) -> Self {
        Self::spawn(credentials, RenewalProof::Proposed(token))
    }

    fn spawn(credentials: OwnerCredentials, authority: RenewalProof) -> Self {
        Self(tokio::spawn(async move {
            let mut renewed = false;
            loop {
                tokio::time::sleep(zenclash_service::OWNER_SESSION_HEARTBEAT_INTERVAL).await;
                let proof = match &authority {
                    RenewalProof::Acknowledged(proof) => proof.clone(),
                    RenewalProof::Proposed(token) => {
                        let Ok(response) = zenclash_service::get_status(&credentials).await else {
                            continue;
                        };
                        let Some(status) = response
                            .data
                            .filter(|status| response.code == 0 && status.is_active)
                        else {
                            continue;
                        };
                        let Some(generation) = status.active_generation else {
                            continue;
                        };
                        // Only the retained proposed token can renew this unconfirmed Start.
                        // The status generation alone never grants controller authority.
                        OwnerSessionProof {
                            generation,
                            token: token.clone(),
                        }
                    }
                };
                match zenclash_service::heartbeat(&credentials, &proof).await {
                    Ok(response) if response.code == 0 => renewed = true,
                    Ok(response)
                        if response.code == ServiceErrorCode::StaleOwnerSession as u16
                            || response.code == ServiceErrorCode::NotActive as u16
                            || response.code == ServiceErrorCode::UnauthorizedOwner as u16 =>
                    {
                        if renewed || matches!(authority, RenewalProof::Acknowledged(_)) {
                            break;
                        }
                    }
                    Ok(response) => {
                        tracing::warn!(code = response.code, "service ownership renewal refused")
                    }
                    Err(error) => tracing::warn!(%error, "service ownership renewal failed"),
                }
            }
        }))
    }
}

impl Drop for SessionKeepalive {
    fn drop(&mut self) {
        self.0.abort();
    }
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

pub use crate::mihomo::errors::ServiceCallError;

/// Owns the native proof for one application session, shared by its UI/controller handles.
/// It does not start a core when constructed; start requires a complete runtime bundle.
pub struct ServiceSession {
    credentials: OwnerCredentials,
    active: RwLock<Option<ActiveServiceSession>>,
    pending_start: RwLock<Option<String>>,
    pending_keepalive: RwLock<Option<SessionKeepalive>>,
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
        let credentials = crate::runtime::service::ipc::owner_credentials(app_root)?;
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
            pending_keepalive: RwLock::new(None),
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
        *self.pending_keepalive.write() = Some(SessionKeepalive::pending(
            self.credentials.clone(),
            proposed_session_token.clone(),
        ));
        let result = match exchange(request).await {
            Ok(result) => result,
            Err(error) => {
                if !error.mutation_result_unknown() {
                    self.pending_start.write().take();
                    self.pending_keepalive.write().take();
                }
                return Err(error);
            }
        };
        // Commit authority before any further await, including optional capability discovery.
        let proof = OwnerSessionProof {
            generation: result.session.generation,
            token: proposed_session_token,
        };
        let keepalive = Arc::new(SessionKeepalive::start(
            self.credentials.clone(),
            proof.clone(),
        ));
        *self.active.write() = Some(ActiveServiceSession {
            proof,
            supports_runtime_staging: false,
            supports_runtime_file_read: false,
            _keepalive: keepalive,
        });
        self.pending_start.write().take();
        self.pending_keepalive.write().take();
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
    /// Read-only queries such as `GET /configs` perform no privileged mutations and do not acquire
    /// the session mutation lock; macOS TUN DNS synchronization is invoked explicitly via
    /// [`Self::sync_tun_dns`] at configuration admission points.
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
    ) -> Result<crate::runtime::service::ipc::NativeHttpResponse, ServiceCallError> {
        #[cfg(target_os = "macos")]
        let _configuration_mutation = if controller_request_requires_mutation_lock(method, path) {
            Some(self.mutations.lock().await)
        } else {
            None
        };
        let (proof, pid, controller) = self.controller_for_owner(secret).await?;
        let response = controller.request(method, path, body, timeout).await?;
        self.confirm_controller_owner(&proof, pid).await?;
        Ok(response)
    }

    /// Synchronizes macOS system DNS state for TUN against the accepted `/configs` readback.
    ///
    /// This privileged step must be called explicitly at configuration admission points
    /// (initial start, hot reload, patch, and recovery) rather than hidden inside read-only
    /// `GET /configs` queries. On non-macOS platforms this is a no-op.
    ///
    /// # Errors
    /// Returns lost ownership, native peer, transport, readback decoding or service-refusal errors.
    pub async fn sync_tun_dns(
        &self,
        secret: &str,
        timeout: std::time::Duration,
    ) -> Result<(), ServiceCallError> {
        #[cfg(target_os = "macos")]
        {
            self.sync_tun_dns_with(
                || async {
                    let (proof, pid, controller) = self.controller_for_owner(secret).await?;
                    let response = controller.request("GET", "/configs", None, timeout).await?;
                    self.confirm_controller_owner(&proof, pid).await?;
                    Ok((proof, pid, response))
                },
                |proof, enabled| async move {
                    let result =
                        zenclash_service::set_tun_dns(&self.credentials, &proof, enabled).await?;
                    check_response(result.code, result.message)
                },
                |proof, pid| async move { self.confirm_controller_owner(&proof, pid).await },
            )
            .await
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (secret, timeout);
            Ok(())
        }
    }

    #[cfg(any(target_os = "macos", test))]
    async fn sync_tun_dns_with<R, RFut, A, AFut, C, CFut>(
        &self,
        read_configs: R,
        apply_dns: A,
        confirm_owner: C,
    ) -> Result<(), ServiceCallError>
    where
        R: FnOnce() -> RFut,
        RFut: std::future::Future<
                Output = Result<
                    (
                        OwnerSessionProof,
                        u32,
                        crate::runtime::service::ipc::NativeHttpResponse,
                    ),
                    ServiceCallError,
                >,
            >,
        A: FnOnce(OwnerSessionProof, bool) -> AFut,
        AFut: std::future::Future<Output = Result<(), ServiceCallError>>,
        C: FnOnce(OwnerSessionProof, u32) -> CFut,
        CFut: std::future::Future<Output = Result<(), ServiceCallError>>,
    {
        let _configuration_mutation = self.mutations.lock().await;
        let (proof, pid, response) = read_configs().await?;
        if !(200..300).contains(&response.status) {
            return Err(ServiceCallError::MissingReply("TUN configuration readback"));
        }
        let config: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|_| ServiceCallError::MissingReply("TUN configuration readback"))?;
        if let Some(enabled) = macos_tun_dns_required(&config) {
            // CRITICAL REGRESSION (2026-10-10): accepted TUN configuration
            // does not mean macOS system DNS enters TUN. LAN DNS can bypass
            // `any:53`, breaking Google/ChatGPT while system proxy works.
            // Keep this privileged readback step; see service core/tun_dns.rs.
            // Every accepted load calls this explicit sync, including initial
            // service handoff, hot reload, patch, and recovery. Refuse to
            // report success if the privileged DNS step failed.
            apply_dns(proof.clone(), enabled).await?;
            confirm_owner(proof, pid).await?;
        }
        Ok(())
    }

    /// Opens a bounded Mihomo native WebSocket; callers retire it with the controller binding.
    ///
    /// # Errors
    /// Returns lost ownership, native peer, connection or WebSocket handshake errors.
    pub async fn controller_socket(
        &self,
        path: &str,
        secret: &str,
    ) -> Result<crate::runtime::service::ipc::NativeSocket, ServiceCallError> {
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
            crate::runtime::service::ipc::controller::NativeController,
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
            crate::runtime::service::ipc::controller::NativeController::new(
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
    pub async fn owner_sample(&self) -> crate::runtime::service::ipc::runstate::OwnerSample {
        use crate::runtime::service::ipc::runstate::OwnerSample;
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
    /// Returns native owner, transport or unconfirmed-Start errors. Only an authenticated native
    /// acknowledgement or confirmed inactive status retires the held proof.
    pub async fn stop(&self) -> Result<(), ServiceCallError> {
        self.stop_with(
            || self.status(),
            |proof| async move {
                let response = zenclash_service::stop_clash(&self.credentials, &proof).await?;
                Ok((response.code, response.message))
            },
        )
        .await
    }

    async fn stop_with<S, SFut, C, CFut>(
        &self,
        read_status: S,
        stop_clash: C,
    ) -> Result<(), ServiceCallError>
    where
        S: FnOnce() -> SFut,
        SFut: std::future::Future<Output = Result<ServiceStatusSnapshot, ServiceCallError>>,
        C: FnOnce(OwnerSessionProof) -> CFut,
        CFut: std::future::Future<Output = Result<(u16, String), ServiceCallError>>,
    {
        let _mutation = self.mutations.lock().await;
        let pending_token = self.pending_start.read().clone();
        if let Some(token) = pending_token {
            let status = read_status().await?;
            if !status.is_active && status.core_pid.is_none() {
                self.clear_all_session_state();
                return Ok(());
            }
            let generation = status
                .active_generation
                .filter(|_| status.is_active)
                .ok_or(ServiceCallError::StartUnconfirmed)?;
            // Status supplies no session token. Use our retained proposed token solely
            // for Stop: the native service verifies both token and generation. Never
            // adopt a controller or stop a replacement merely because its SID matches.
            let proof = OwnerSessionProof { generation, token };
            let (code, message) = stop_clash(proof).await?;
            if code > 0 && code != ServiceErrorCode::NotActive as u16 {
                return Err(ServiceCallError::Rejected { code, message });
            }
            self.clear_all_session_state();
            return Ok(());
        }
        let Some(session) = self.active.read().clone() else {
            return Ok(());
        };
        let (code, message) = stop_clash(session.proof).await?;
        if code > 0
            && code != ServiceErrorCode::NotActive as u16
            && code != ServiceErrorCode::StaleOwnerSession as u16
        {
            return Err(ServiceCallError::Rejected { code, message });
        }
        let retired = self.active.write().take();
        self.observed_status.write().take();
        drop(retired);
        Ok(())
    }

    /// Retires local session and pending-start keepalive state after a fresh status observation
    /// independently proves the service is inactive or the captured generation is gone.
    pub(crate) fn retire_confirmed_stop(
        &self,
        expected_generation: Option<u64>,
        status: &ServiceStatusSnapshot,
    ) {
        if !status.is_active && status.core_pid.is_none() {
            self.clear_all_session_state();
            return;
        }
        if let Some(generation) = expected_generation
            && status.active_generation != Some(generation)
        {
            let retired = {
                let mut active = self.active.write();
                if active
                    .as_ref()
                    .is_some_and(|session| session.proof.generation == generation)
                {
                    active.take()
                } else {
                    None
                }
            };
            self.observed_status.write().take();
            drop(retired);
        }
    }

    fn clear_all_session_state(&self) {
        self.pending_start.write().take();
        let pending_keepalive = self.pending_keepalive.write().take();
        let active = self.active.write().take();
        self.observed_status.write().take();
        drop((pending_keepalive, active));
    }

    #[cfg(test)]
    pub(crate) fn disconnected_for_test() -> Self {
        Self {
            credentials: OwnerCredentials {
                identity: zenclash_service::OwnerIdentity::Unix { uid: 0, gid: 0 },
                app_data_dir: String::new(),
                token: None,
            },
            active: RwLock::new(None),
            pending_start: RwLock::new(None),
            pending_keepalive: RwLock::new(None),
            observed_status: RwLock::new(None),
            mutations: Mutex::new(()),
        }
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

#[cfg(any(target_os = "macos", test))]
fn controller_request_requires_mutation_lock(method: &str, path: &str) -> bool {
    !matches!(method, "GET" | "HEAD") && path.split('?').next() == Some("/configs")
}

#[cfg(any(target_os = "macos", test))]
fn macos_tun_dns_required(config: &serde_json::Value) -> Option<bool> {
    let tun = config.get("tun")?;
    if !tun.get("enable")?.as_bool()? {
        return Some(false);
    }
    // Respect custom routing and DNS capture. The temporary resolver is useful
    // only if Mihomo can intercept UDP DNS sent to its fixed public address.
    let captures_dns = tun.get("dns-hijack")?.as_array()?.iter().any(|value| {
        matches!(
            value.as_str(),
            Some(
                "any:53"
                    | "udp://any:53"
                    | "0.0.0.0:53"
                    | "udp://0.0.0.0:53"
                    | "1.1.1.1:53"
                    | "udp://1.1.1.1:53"
            )
        )
    });
    Some(tun.get("auto-route").and_then(serde_json::Value::as_bool) != Some(false) && captures_dns)
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
mod tests;

#[cfg(all(test, feature = "service-ipc-tests"))]
mod native_pending_tests;
