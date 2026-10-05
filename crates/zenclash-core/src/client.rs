//! Typed access to Mihomo's external-controller HTTP API.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CoreConfigValidator, CoreKind, MihomoEndpoint, MihomoProcess, owned_core::OwnedCore};

mod api;
pub(crate) use api::AppliedConfig;
mod connections;
mod request;
mod transport;

pub(crate) use transport::{ControllerBinding, ControllerStream};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ownership_tests;

#[cfg(test)]
mod startup_tests;

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
    /// Authenticated native service transport or policy rejection.
    #[error("Mihomo service request failed: {0}")]
    Service(#[from] zenclash_service_integration::ServiceCallError),
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
}

impl MihomoError {
    /// Returns a definitive native startup rejection, if one was observed.
    /// Transport loss and timeouts remain unknown; this reader performs no IPC.
    #[must_use]
    pub fn service_startup_rejection(&self) -> Option<ServiceStartupRejection> {
        use zenclash_service::ServiceErrorCode;
        use zenclash_service_integration::ServiceCallError;
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

/// Cloneable HTTP client for Mihomo's external-controller API.
#[derive(Clone)]
pub struct MihomoClient {
    pub(crate) binding: Arc<ControllerBinding>,
    http: reqwest::Client,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    config_validator: Option<(u64, CoreConfigValidator)>,
    pinned_binding: Option<transport::BindingSnapshot>,
    connections: Arc<connections::ConnectionCache>,
    pub(crate) delay_gate: Arc<tokio::sync::Semaphore>,
}

impl MihomoClient {
    /// Verifies the service and loads credentials for the ordinary application root.
    /// Construction does not acquire a generation or start a core; applying a complete
    /// runtime performs the native Start operation and retains the returned proof.
    ///
    /// # Errors
    /// Returns client-construction, native identity, authorization, ownership,
    /// incompatible-protocol or transport errors. No local core is started.
    pub async fn connect_service(source_home: std::path::PathBuf) -> MihomoResult<Self> {
        Self::connect_service_with_core(source_home, None).await
    }

    /// Connects using the same selected core as installation health detection.
    ///
    /// # Errors
    /// Returns native identity, protocol or transport errors without starting a core.
    pub async fn connect_service_with_core(
        source_home: std::path::PathBuf,
        core_source: Option<std::path::PathBuf>,
    ) -> MihomoResult<Self> {
        let http = Self::build_http()?;
        let service =
            Arc::new(zenclash_service_integration::ServiceSession::connect(&source_home).await?);
        Ok(Self::with_http(
            ControllerBinding::service_binding_with_core(service, source_home, core_source),
            http,
        ))
    }

    /// Creates a client with bounded connection and request timeouts.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(endpoint: MihomoEndpoint) -> MihomoResult<Self> {
        Self::new_binding(ControllerBinding::direct(endpoint))
    }

    /// Creates a client using verified native IPC and a user-readable resource home.
    ///
    /// # Errors
    /// Returns an error if the HTTP client used for future Direct bindings cannot be built.
    pub fn from_service(
        service: Arc<zenclash_service_integration::ServiceSession>,
        source_home: std::path::PathBuf,
    ) -> MihomoResult<Self> {
        Self::new_binding(ControllerBinding::service_binding(service, source_home))
    }

    /// Creates a client retaining the actual managed child and its controller.
    /// All clones share this ownership and transport through one binding.
    ///
    /// # Errors
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn from_process(process: Arc<MihomoProcess>) -> MihomoResult<Self> {
        Self::new_binding(ControllerBinding::process_binding(process))
    }

    fn new_binding(binding: Arc<ControllerBinding>) -> MihomoResult<Self> {
        Ok(Self::with_http(binding, Self::build_http()?))
    }

    fn build_http() -> MihomoResult<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?)
    }

    fn with_http(binding: Arc<ControllerBinding>, http: reqwest::Client) -> Self {
        Self {
            binding,
            http,
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            config_validator: None,
            pinned_binding: None,
            connections: Arc::default(),
            delay_gate: Arc::new(tokio::sync::Semaphore::new(16)),
        }
    }

    /// Selects the external runtime kind during unique, unused binding construction.
    ///
    /// # Errors
    /// Rejects a different owned kind or changing kind after sharing or using the binding.
    pub fn with_core_kind(mut self, kind: CoreKind) -> MihomoResult<Self> {
        if self.binding.descriptor().kind() != kind {
            Arc::get_mut(&mut self.binding)
                .ok_or_else(|| {
                    MihomoError::InvalidInput(zenclash_i18n::text(
                        "core_page.errors.runtime_kind_locked",
                    ))
                })?
                .initialize_kind(kind)?;
        }
        Ok(self)
    }

    /// Enables executable validation for the same initially declared runtime kind.
    ///
    /// # Errors
    /// Rejects validators for a different actual runtime implementation.
    pub fn with_config_validator(mut self, validator: CoreConfigValidator) -> MihomoResult<Self> {
        self = self.with_core_kind(validator.kind())?;
        self.config_validator = Some((self.binding.generation(), validator));
        Ok(self)
    }

    pub(crate) fn write_scopes(&self) -> Vec<std::path::PathBuf> {
        let mut scopes = self
            .current_config_validator()
            .map_or_else(Vec::new, |validator| validator.write_scopes());
        match self.owned_core() {
            Some(OwnedCore::Local(process)) => {
                // Restart also validates the actual launch config, which can be outside home.
                scopes.extend(process.write_scopes());
            }
            Some(OwnedCore::Service(runtime)) => scopes.push(runtime.source_home().to_path_buf()),
            None => {}
        }
        scopes
    }

    pub(crate) fn pin_binding(&self) -> MihomoResult<Self> {
        Ok(Self {
            pinned_binding: Some(self.operation_binding()?),
            ..self.clone()
        })
    }

    pub(crate) fn ensure_binding_current(&self) -> MihomoResult<()> {
        self.operation_binding().map(|_| ())
    }

    pub(crate) fn mark_runtime_binding_used(&self) {
        self.binding.mark_used();
    }

    pub(crate) fn close_runtime_admission(&self) {
        self.binding.close_admission();
    }

    pub(crate) fn runtime_descriptor(&self) -> crate::owned_core::CoreRuntimeDescriptor {
        self.binding.descriptor()
    }

    pub(crate) fn local_recovery_launch(&self) -> Option<crate::MihomoLaunchConfig> {
        self.binding.local_recovery_launch()
    }

    pub(crate) async fn lock_runtime_binding(
        &self,
    ) -> MihomoResult<tokio::sync::OwnedMutexGuard<()>> {
        let guard = self.mutation_gate.clone().lock_owned().await;
        self.ensure_binding_current()?;
        Ok(guard)
    }

    pub(super) fn operation_binding(&self) -> MihomoResult<transport::BindingSnapshot> {
        self.binding.mark_used();
        let binding = self.binding_snapshot();
        if !self.binding.is_current(binding.generation) {
            return Err(MihomoError::StaleBinding);
        }
        Ok(binding)
    }

    fn binding_snapshot(&self) -> transport::BindingSnapshot {
        self.pinned_binding
            .clone()
            .unwrap_or_else(|| self.binding.snapshot())
    }

    pub(crate) fn with_write_lease(
        &self,
        lease: &crate::data_coordinator::DataWriteLease,
    ) -> MihomoResult<Self> {
        let binding = self.operation_binding()?;
        let validator = self.validator_for_binding(&binding);
        if validator.as_ref().is_some_and(|validator| {
            validator
                .write_scopes()
                .iter()
                .any(|path| !lease.covers(path))
        }) {
            return Err(MihomoError::StaleBinding);
        }
        Ok(Self {
            config_validator: self
                .validator_for_binding(&binding)
                .map(|validator| (binding.generation, validator.with_write_lease(lease))),
            pinned_binding: Some(binding),
            ..self.clone()
        })
    }

    async fn acquire_write_lease(
        &self,
    ) -> MihomoResult<Option<crate::data_coordinator::DataWriteLease>> {
        match self.current_config_validator() {
            Some(validator) => validator
                .acquire_write_lease()
                .await
                .map(Some)
                .map_err(MihomoError::Process),
            None => Ok(None),
        }
    }

    pub(crate) fn current_config_validator(&self) -> Option<CoreConfigValidator> {
        self.validator_for_binding(&self.binding_snapshot())
    }

    fn validator_for_binding(
        &self,
        binding: &transport::BindingSnapshot,
    ) -> Option<CoreConfigValidator> {
        let explicit = self
            .config_validator
            .as_ref()
            .and_then(|(generation, validator)| {
                (*generation == binding.generation).then_some(validator)
            });
        match &binding.backend {
            transport::ControllerBackend::Local(process) => {
                let actual = process.config_validator();
                // Preserve a temporary write lease only when it authorizes this actual child.
                Some(match explicit {
                    Some(validator)
                        if validator.kind() == process.kind()
                            && validator.write_scopes() == actual.write_scopes() =>
                    {
                        validator.clone()
                    }
                    _ => actual,
                })
            }
            transport::ControllerBackend::Direct(_) => explicit.cloned(),
            transport::ControllerBackend::Service { .. } => None,
        }
    }

    fn current_kind(&self) -> CoreKind {
        self.binding_snapshot().kind
    }

    pub(crate) fn binding_snapshot_kind(&self) -> CoreKind {
        self.binding_snapshot().kind
    }

    pub(crate) fn owned_core(&self) -> Option<OwnedCore> {
        self.binding_snapshot().owned_core()
    }

    pub(crate) fn normalize_config_payload(&self, payload: String) -> MihomoResult<String> {
        self.ensure_binding_current()?;
        if payload.trim().is_empty() {
            return Err(MihomoError::InvalidInput("重载配置内容不能为空".into()));
        }
        if payload.len() > crate::profiles::MAX_PROFILE_BYTES {
            return Err(MihomoError::InvalidInput(format!(
                "重载配置超过 {} MiB 限制",
                crate::profiles::MAX_PROFILE_BYTES / 1024 / 1024
            )));
        }
        crate::controlled_config::normalize_runtime_payload(self.current_kind(), payload)
            .map_err(|error| MihomoError::InvalidInput(error.to_string()))
    }

    /// Returns the ordinary controller endpoint, or `None` for native service IPC.
    #[must_use]
    pub fn endpoint(&self) -> Option<MihomoEndpoint> {
        self.binding.endpoint()
    }

    /// Returns the authenticated service owner without exposing its controller.
    #[must_use]
    pub fn service_client(&self) -> Option<Arc<zenclash_service_integration::ServiceSession>> {
        self.runtime_session().map(|runtime| runtime.client.clone())
    }

    pub(super) fn runtime_session(
        &self,
    ) -> Option<Arc<crate::service_runtime_session::ServiceRuntimeSession>> {
        match self.owned_core() {
            Some(OwnedCore::Service(runtime)) => Some(runtime),
            Some(OwnedCore::Local(_)) | None => None,
        }
    }

    /// Commits a service transport to every clone after pending mutations finish.
    /// The caller coordinates process stop/start and business generation first.
    ///
    /// # Errors
    /// Rejects non-Mihomo clients or exhausted binding generations.
    pub(crate) async fn switch_to_service(
        &self,
        service: Arc<zenclash_service_integration::ServiceSession>,
        source_home: std::path::PathBuf,
    ) -> MihomoResult<()> {
        if self.current_kind() != CoreKind::Mihomo {
            return Err(MihomoError::InvalidInput("服务仅支持 Mihomo 内核".into()));
        }
        let guard = self.mutation_gate.clone().lock_owned().await;
        self.ensure_binding_current()?;
        self.binding.ensure_open()?;
        if let Some(runtime) = self.binding.runtime() {
            if Arc::ptr_eq(&runtime.client, &service) {
                if runtime.source_home() != source_home {
                    return Err(MihomoError::InvalidInput(
                        "服务资源目录不能在同一会话内更改".into(),
                    ));
                }
                runtime.reconcile().await?;
                return Ok(());
            }
            runtime.release_owned().await?;
        }
        let runtime = match self
            .binding
            .local_recovery_launch()
            .filter(|launch| launch.home_dir == source_home)
        {
            Some(launch) => {
                crate::service_runtime_session::ServiceRuntimeSession::from_local(service, launch)
            }
            None => {
                crate::service_runtime_session::ServiceRuntimeSession::new(service, source_home)
            }
        };
        self.publish_backend(transport::ControllerBackend::Service { runtime }, guard)
            .await
    }

    /// Publishes a real managed child and its controller to every existing clone.
    /// The caller coordinates confirmed stop/start before changing ownership.
    /// Retired local children are stopped on a blocking worker before completion.
    ///
    /// # Errors
    /// Rejects a different core kind, unresolved service mutation, or exhausted generation.
    pub(crate) async fn switch_to_process(&self, process: Arc<MihomoProcess>) -> MihomoResult<()> {
        let guard = self.mutation_gate.clone().lock_owned().await;
        self.ensure_binding_current()?;
        self.binding.ensure_open()?;
        if process.kind() != self.current_kind() {
            return Err(MihomoError::InvalidInput(
                "绑定的进程与客户端内核类型不一致".into(),
            ));
        }
        if let Some(runtime) = self.binding.runtime() {
            runtime.release_owned().await?;
        }
        self.publish_backend(transport::ControllerBackend::Local(process), guard)
            .await
    }

    /// Commits an ordinary controller transport to every existing client clone.
    ///
    /// # Errors
    /// Returns an error if the binding generation is exhausted.
    pub(crate) async fn switch_to_direct(&self, endpoint: MihomoEndpoint) -> MihomoResult<()> {
        let guard = self.mutation_gate.clone().lock_owned().await;
        self.ensure_binding_current()?;
        self.binding.ensure_open()?;
        if let Some(runtime) = self.binding.runtime() {
            runtime.release_owned().await?;
        }
        self.publish_backend(transport::ControllerBackend::Direct(endpoint), guard)
            .await
    }

    pub(crate) async fn publish_prepared_service(
        &self,
        runtime: Arc<crate::service_runtime_session::ServiceRuntimeSession>,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<tokio::sync::OwnedMutexGuard<()>> {
        self.publish_owned_backend(transport::ControllerBackend::Service { runtime }, guard)
            .await
    }

    pub(crate) async fn publish_prepared_process(
        &self,
        process: Arc<MihomoProcess>,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<tokio::sync::OwnedMutexGuard<()>> {
        self.publish_owned_backend(transport::ControllerBackend::Local(process), guard)
            .await
    }

    async fn publish_backend(
        &self,
        backend: transport::ControllerBackend,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<()> {
        self.publish_owned_backend(backend, guard).await.map(drop)
    }

    async fn publish_owned_backend(
        &self,
        backend: transport::ControllerBackend,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> MihomoResult<tokio::sync::OwnedMutexGuard<()>> {
        self.binding.ensure_open()?;
        let replacement = match &backend {
            transport::ControllerBackend::Local(process) => Some(process.clone()),
            _ => None,
        };
        let retired = self.binding.replace(backend)?;
        self.invalidate_connections();
        tokio::task::spawn_blocking(move || {
            let result = match &retired.backend {
                transport::ControllerBackend::Local(process)
                    if !replacement
                        .as_ref()
                        .is_some_and(|next| Arc::ptr_eq(process, next)) =>
                {
                    process.stop()
                }
                _ => Ok(()),
            };
            drop(retired);
            drop(replacement);
            // Keep publication admission even if the awaiting caller is cancelled.
            // At most one retirement task can exist for this shared binding.
            result.map(|()| guard)
        })
        .await
        .map_err(|error| {
            MihomoError::Process(zenclash_i18n::text_with(
                "core_page.errors.binding_retirement",
                &[("error", error.to_string())],
            ))
        })?
    }

    /// Finishes application acceptance after configuration was durably saved.
    /// A pending saved snapshot is reapplied through native staging if necessary.
    ///
    /// # Errors
    /// Returns an error if ownership or the actual controller cannot be confirmed.
    pub async fn reconcile_service_runtime(&self) -> MihomoResult<()> {
        let client = self.pin_binding()?;
        let _guard = client.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let runtime = client.runtime_session().ok_or(MihomoError::StaleBinding)?;
        runtime.reconcile().await
    }

    /// Restores a service revision whose candidate was not durably committed.
    /// A pending finalization is rejected because its user data is already saved.
    ///
    /// # Errors
    /// Returns transport errors or a missing accepted snapshot after stopping the kernel.
    pub async fn rollback_service_runtime(&self) -> MihomoResult<()> {
        let client = self.pin_binding()?;
        let _guard = client.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let runtime = client.runtime_session().ok_or(MihomoError::StaleBinding)?;
        runtime.restore_active().await
    }

    pub(crate) fn service_runtime_snapshot(
        &self,
    ) -> MihomoResult<Option<Arc<crate::ServiceRuntimeBundle>>> {
        self.ensure_binding_current()?;
        self.runtime_session()
            .map(|runtime| runtime.snapshot())
            .transpose()
    }
}

impl MihomoError {
    /// Reports whether a failed mutation may already have changed the controller.
    /// Policy/input rejection and failure to establish native IPC are definitive.
    #[must_use]
    pub fn mutation_result_unknown(&self) -> bool {
        match self {
            Self::Http(_)
            | Self::StaleTransport
            | Self::RuntimeOutcomeUnknown
            | Self::RuntimePartiallyApplied(_) => true,
            Self::Service(error) => error.mutation_result_unknown(),
            _ => false,
        }
    }
}
