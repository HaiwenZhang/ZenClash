//! Transactional coordination of System Proxy and TUN capture plans.

use std::{
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::RwLock;

use crate::{
    ControlledConfigStore, CoreSession, Observation, SystemProxyOwnershipState, SystemProxySession,
    SystemProxySessionSnapshot, TunCaptureStatus, runtime::session::RuntimeConfigurationRevision,
};

mod backend;
use backend::ProductionCaptureBackend;
mod core_recovery;
mod error;
use error::task_failure;
pub use error::{TrafficCaptureError, TrafficCaptureTask};
mod plans;
#[cfg(test)]
use plans::failed_snapshot;
pub use plans::{
    CaptureOutcome, CapturePlan, ObservedCapturePlan, ServiceLocalRecoveryOutcome,
    ServiceTunOutcome, TrafficCaptureSnapshot,
};
use plans::{
    has_external_system_proxy, system_proxy_has_intent_or_ownership,
    system_proxy_is_owned_and_active,
};
mod transitions;

type CaptureFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TrafficCaptureError>> + Send + 'a>>;

/// Deep module serializing capture plan changes and partial-failure recovery.
#[derive(Clone)]
pub struct TrafficCaptureSession {
    backend: Arc<dyn CaptureBackend>,
    profile: Arc<RwLock<Option<PathBuf>>>,
    operation: Arc<tokio::sync::Mutex<()>>,
    intent_revision: Arc<AtomicU64>,
}

impl TrafficCaptureSession {
    /// Creates a production capture session over the existing runtime and platform owners.
    #[must_use]
    pub fn new(
        core_session: CoreSession,
        controlled: ControlledConfigStore,
        system_proxy: Option<SystemProxySession>,
        profile: Option<PathBuf>,
    ) -> Self {
        let operation = core_session.capture_publication_gate();
        let intent_revision = core_session.capture_intent_revision();
        let profile = Arc::new(RwLock::new(profile));
        let backend = Arc::new(ProductionCaptureBackend::new(
            core_session,
            controlled,
            system_proxy,
            profile.clone(),
        ));
        Self {
            backend,
            profile,
            operation,
            intent_revision,
        }
    }

    pub(crate) fn controlled_store(&self) -> Option<ControlledConfigStore> {
        self.backend.controlled_store()
    }

    pub(crate) fn current_profile(&self) -> Option<PathBuf> {
        self.profile.read().clone()
    }

    pub(crate) fn note_capture_intent(&self) {
        let _ = self
            .intent_revision
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |revision| {
                Some(revision.saturating_add(1))
            });
    }

    pub(crate) fn capture_revision(&self) -> u64 {
        self.intent_revision.load(Ordering::SeqCst)
    }

    /// Reads the current user capture intent version without native operations.
    #[must_use]
    pub fn intent_revision(&self) -> u64 {
        self.capture_revision()
    }

    pub(crate) fn shares_core_session(&self, session: &CoreSession) -> bool {
        Arc::ptr_eq(&self.operation, &session.capture_publication_gate())
    }

    /// Updates the active profile used by subsequent TUN transitions.
    pub fn set_profile(&self, profile: Option<PathBuf>) {
        *self.profile.write() = profile;
    }

    /// Releases owned native capture and recovers the service into ordinary Local ownership.
    ///
    /// Captures intent under the shared publication gate. Once admitted, the
    /// completion survives cancellation of its caller and shutdown waits for it.
    /// Persists disabled system-proxy intent through its existing transaction before
    /// leaving Service. Does not grant maintenance authorization or restore capture
    /// after failure; an uncertain runtime must first be reconciled.
    ///
    /// # Errors
    /// Rejects mismatched capture owners, shutdown, stale versions, non-service
    /// runtimes, native capture release failure or failed Local recovery.
    pub async fn recover_service_to_local(
        &self,
        session: &CoreSession,
        store: &ControlledConfigStore,
        fallback: Option<crate::CoreLaunchConfig>,
        expected_binding: u64,
        expected_generation: u64,
    ) -> Result<ServiceLocalRecoveryOutcome, crate::CoreSessionError> {
        self.recover_service_to_local_with_revision(
            session,
            store,
            fallback,
            RuntimeConfigurationRevision {
                binding_generation: expected_binding,
                configuration_generation: expected_generation,
            },
        )
        .await
    }

    pub(crate) async fn recover_service_to_local_with_revision(
        &self,
        session: &CoreSession,
        store: &ControlledConfigStore,
        fallback: Option<crate::CoreLaunchConfig>,
        expected_runtime: RuntimeConfigurationRevision,
    ) -> Result<ServiceLocalRecoveryOutcome, crate::CoreSessionError> {
        if !self.shares_core_session(session) {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        let capture_revision = self.capture_revision();
        let capture = self.operation.clone().lock_owned().await;
        if self.capture_revision() != capture_revision {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        if session.is_shutting_down() {
            return Err(crate::CoreSessionError::ShuttingDown);
        }
        let descriptor = session.runtime_descriptor();
        if descriptor.binding_generation() != expected_runtime.binding_generation
            || session.generation() != expected_runtime.configuration_generation
        {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        if descriptor.backend() != crate::CoreRuntimeBackend::Service {
            return Err(crate::CoreSessionError::ReleaseUnsupported {
                core: session.kind(),
            });
        }
        let owner = self.clone();
        let session = session.clone();
        let store = store.clone();
        tokio::spawn(async move {
            let before = owner
                .snapshot_from_backend()
                .await
                .map_err(crate::CoreSessionError::from)?;
            owner
                .suspend_maintenance_proxy_admitted(&before.system_proxy)
                .await
                .map_err(crate::CoreSessionError::from)?;
            if session.is_shutting_down() {
                return Err(crate::CoreSessionError::ShuttingDown);
            }
            if session.runtime_descriptor().binding_generation()
                != expected_runtime.binding_generation
                || session.generation() != expected_runtime.configuration_generation
            {
                return Err(crate::MihomoError::StaleBinding.into());
            }
            let runtime = session
                .recover_service_to_local_admitted(
                    &store,
                    fallback,
                    capture,
                    expected_runtime.configuration_generation,
                )
                .await?;
            Ok(ServiceLocalRecoveryOutcome {
                before,
                runtime,
                capture_revision,
            })
        })
        .await
        .map_err(|source| {
            crate::CoreSessionError::from(task_failure(
                TrafficCaptureTask::ServiceLocalRecovery,
                source,
            ))
        })?
    }

    pub(crate) async fn suspend_maintenance_proxy_admitted(
        &self,
        before: &Observation<SystemProxySessionSnapshot>,
    ) -> Result<(), TrafficCaptureError> {
        if !before.is_fresh() {
            return Err(TrafficCaptureError::ObservationUnconfirmed);
        }
        if before.value().is_some_and(|proxy| {
            proxy.intent_enabled || proxy.ownership == SystemProxyOwnershipState::Owned
        }) {
            self.backend.set_system_proxy(false, 0).await
        } else {
            Ok(())
        }
    }

    pub(crate) async fn restore_maintenance_proxy(
        &self,
        session: &CoreSession,
        before: TrafficCaptureSnapshot,
        expected_runtime: RuntimeConfigurationRevision,
        expected_capture_revision: u64,
    ) -> Result<(), crate::CoreSessionError> {
        if !self.shares_core_session(session) {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        let guard = self.operation.clone().lock_owned().await;
        let owner = self.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let _guard = guard;
            if session.is_shutting_down() {
                return Err(crate::CoreSessionError::ShuttingDown);
            }
            if session.runtime_descriptor().binding_generation()
                != expected_runtime.binding_generation
                || session.generation() != expected_runtime.configuration_generation
                || owner.intent_revision.load(Ordering::SeqCst) != expected_capture_revision
            {
                return Err(crate::MihomoError::StaleBinding.into());
            }
            owner
                .restore_maintenance_proxy_admitted(&before, expected_capture_revision)
                .await
                .map_err(crate::CoreSessionError::from)
        })
        .await
        .map_err(|source| {
            crate::CoreSessionError::from(task_failure(
                TrafficCaptureTask::RestoreMaintenanceProxy,
                source,
            ))
        })?
    }

    async fn restore_maintenance_proxy_admitted(
        &self,
        before: &TrafficCaptureSnapshot,
        expected_revision: u64,
    ) -> Result<(), TrafficCaptureError> {
        if !before.system_proxy.is_fresh() {
            return Err(TrafficCaptureError::ObservationUnconfirmed);
        }
        if !system_proxy_is_owned_and_active(before) {
            return Ok(());
        }
        let current = self.snapshot_from_backend().await?;
        if !current.system_proxy.is_fresh() || !current.core_available {
            return Err(TrafficCaptureError::ObservationUnconfirmed);
        }
        if has_external_system_proxy(&current) {
            return Err(TrafficCaptureError::ExternalSystemProxy);
        }
        let port = current
            .system_proxy_port
            .filter(|port| *port != 0)
            .ok_or_else(|| TrafficCaptureError::ObservationUnconfirmed)?;
        if self.capture_revision() != expected_revision {
            return Err(TrafficCaptureError::Controller(
                crate::MihomoError::StaleBinding,
            ));
        }
        self.backend.set_system_proxy(true, port).await?;
        let after = self.snapshot_from_backend().await?;
        if !after.system_proxy.is_fresh() || !system_proxy_is_owned_and_active(&after) {
            return Err(TrafficCaptureError::ObservationUnconfirmed);
        }
        Ok(())
    }

    /// Enables TUN through one admitted service handover and configuration transaction.
    /// `Some` supplies an authenticated helper for a local handover; `None` reuses the current service owner.
    ///
    /// # Errors
    /// Rejects stale intent, shutdown, unsupported ownership, missing accepted configuration,
    /// or failures before a recoverable native transition. Saved or uncertain results carry capture facts.
    pub async fn enable_service_tun(
        &self,
        service: Option<(Arc<crate::runtime::service::ipc::ServiceSession>, PathBuf)>,
        expected_binding: u64,
        expected_generation: u64,
    ) -> Result<ServiceTunOutcome, crate::CoreSessionError> {
        self.note_capture_intent();
        self.enable_service_tun_with_bundle(
            service,
            RuntimeConfigurationRevision {
                binding_generation: expected_binding,
                configuration_generation: expected_generation,
            },
            None,
            None,
            None,
        )
        .await
    }

    pub(crate) async fn enable_service_tun_with_bundle(
        &self,
        service: Option<(Arc<crate::runtime::service::ipc::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        expected_capture_revision: Option<u64>,
        prepared_config: Option<Arc<crate::runtime::session::service_tun::PreparedServiceTun>>,
    ) -> Result<ServiceTunOutcome, crate::CoreSessionError> {
        let session = self.clone();
        // The acquired helper belongs to this completion even while capture is queued.
        // Manager command admission bounds this workflow to one outstanding authorization.
        tokio::spawn(async move {
            let _guard = session.operation.clone().lock_owned().await;
            let acquired = service.as_ref().map(|(client, _)| client.clone());
            let result = session
                .enable_service_tun_admitted(
                    service,
                    expected_runtime,
                    recovery_bundle,
                    expected_capture_revision,
                    prepared_config,
                )
                .await;
            if let Err(original_error) = &result
                && let Some(acquired) = acquired
                && !session.backend.owns_service(&acquired)
                && let Err(cleanup_error) = session.backend.stop_unowned_service(acquired).await
            {
                tracing::warn!(
                    %original_error,
                    %cleanup_error,
                    "failed to stop unowned service session after service TUN admission error"
                );
            }
            result
        })
        .await
        .map_err(|source| {
            crate::CoreSessionError::from(task_failure(
                TrafficCaptureTask::EnableServiceTun,
                source,
            ))
        })?
    }

    async fn enable_service_tun_admitted(
        &self,
        service: Option<(Arc<crate::runtime::service::ipc::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        expected_capture_revision: Option<u64>,
        prepared_config: Option<Arc<crate::runtime::session::service_tun::PreparedServiceTun>>,
    ) -> Result<ServiceTunOutcome, crate::CoreSessionError> {
        if expected_capture_revision.is_some_and(|expected| expected != self.capture_revision()) {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        self.backend
            .validate_service_tun_request(expected_runtime)?;
        let before = self
            .snapshot_from_backend()
            .await
            .map_err(crate::CoreSessionError::from)?;
        if expected_capture_revision.is_some_and(|expected| expected != self.capture_revision()) {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        if has_external_system_proxy(&before) {
            return Err(crate::CoreSessionError::from(
                TrafficCaptureError::ExternalSystemProxy,
            ));
        }
        // Proxy ownership stays with capture; its previous snapshot remains held through core recovery.
        if system_proxy_has_intent_or_ownership(&before) {
            self.backend
                .set_system_proxy(false, 0)
                .await
                .map_err(crate::CoreSessionError::from)?;
        }
        let runtime = self
            .backend
            .enable_service_tun(service, expected_runtime, recovery_bundle, prepared_config)
            .await;
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                if system_proxy_is_owned_and_active(&before) {
                    let rollback = self.restore_system_proxy(&before).await;
                    if let Err(rollback) = rollback {
                        return Err(crate::CoreSessionError::from(
                            TrafficCaptureError::Rollback {
                                cause: Box::new(error),
                                rollback: Box::new(rollback),
                            },
                        ));
                    }
                }
                return Err(error);
            }
        };
        let mut recovery_warning = runtime.recovery_warning;
        let capture = if let Some(failure) = runtime.failure {
            let rollback = if runtime.restored && system_proxy_is_owned_and_active(&before) {
                self.restore_system_proxy(&before).await
            } else if runtime.restored {
                Ok(())
            } else {
                Err(TrafficCaptureError::RuntimeRecoveryUnconfirmed)
            };
            self.failed_outcome(CapturePlan::Tun, failure.into(), rollback)
                .await
        } else {
            // Readback failure after a durable save must retain the saved receipt.
            match self.applied(CapturePlan::Tun, &before).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    recovery_warning = Some(error.to_string());
                    CaptureOutcome::ReconcileNeeded {
                        plan: Some(CapturePlan::Tun),
                        failure: error.to_string(),
                        snapshot: self.best_effort_snapshot().await,
                    }
                }
            }
        };
        Ok(ServiceTunOutcome {
            core: runtime.saved,
            capture,
            commit_pending: runtime.commit_pending,
            recovery_warning,
        })
    }

    #[cfg(all(test, feature = "service-ipc-tests"))]
    pub(crate) fn with_unowned_capture_for_test(core: &CoreSession) -> Self {
        let mut capture = Self::with_backend(Arc::new(tests::FakeBackend::unowned_for_test()));
        capture.operation = core.capture_publication_gate();
        capture.intent_revision = core.capture_intent_revision();
        capture
    }

    #[cfg(test)]
    fn with_backend(backend: Arc<dyn CaptureBackend>) -> Self {
        Self {
            backend,
            profile: Arc::default(),
            operation: Arc::default(),
            intent_revision: Arc::default(),
        }
    }
}

#[derive(Debug)]
struct CaptureBackendSnapshot {
    system_proxy: Result<SystemProxySessionSnapshot, TrafficCaptureError>,
    tun: Result<TunCaptureStatus, TrafficCaptureError>,
    system_proxy_port: Option<u16>,
    core_available: bool,
}

trait CaptureBackend: Send + Sync {
    fn controlled_store(&self) -> Option<ControlledConfigStore> {
        None
    }

    fn owns_service(&self, _service: &Arc<crate::runtime::service::ipc::ServiceSession>) -> bool {
        false
    }

    fn stop_unowned_service(
        &self,
        service: Arc<crate::runtime::service::ipc::ServiceSession>,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), crate::runtime::service::ipc::ServiceCallError>> + Send>,
    > {
        Box::pin(async move { service.stop().await })
    }
    fn validate_service_tun_request(
        &self,
        _expected_runtime: RuntimeConfigurationRevision,
    ) -> Result<(), crate::CoreSessionError> {
        Ok(())
    }

    fn enable_service_tun(
        &self,
        _service: Option<(Arc<crate::runtime::service::ipc::ServiceSession>, PathBuf)>,
        _expected_runtime: RuntimeConfigurationRevision,
        _recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        _prepared_config: Option<Arc<crate::runtime::session::service_tun::PreparedServiceTun>>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        crate::runtime::session::service_tun::ServiceTunRuntimeOutcome,
                        crate::CoreSessionError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(crate::CoreSessionError::ReleaseUnsupported {
                core: crate::CoreKind::Mihomo,
            })
        })
    }

    fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot>;
    fn resync_runtime_profile(&self) -> CaptureFuture<'_, ()>;
    fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()>;
    fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()>;
    fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()>;
    fn close_owned_connections(&self) -> CaptureFuture<'_, ()>;
    fn reconcile(&self) -> CaptureFuture<'_, ()>;
    fn release_owned(&self) -> CaptureFuture<'_, ()>;
}

#[cfg(test)]
mod tests;
