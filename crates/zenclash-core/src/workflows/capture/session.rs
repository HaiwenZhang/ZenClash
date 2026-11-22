//! Transactional coordination of System Proxy and TUN capture plans.

use crate::runtime::session::RuntimeConfigurationRevision;
use std::{
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
mod backend;
use backend::ProductionCaptureBackend;
mod core_recovery;
mod error;
use error::task_failure;
pub use error::{TrafficCaptureError, TrafficCaptureTask};

use crate::{
    ControlledConfigStore, CoreSession, Observation, RecoveryAction, SystemProxyOwnershipState,
    SystemProxySession, SystemProxySessionSnapshot, TunCaptureStatus,
};

type CaptureFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TrafficCaptureError>> + Send + 'a>>;

/// Mutually exclusive capture choice offered by ordinary UI surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePlan {
    /// Disable ZenClash-owned System Proxy and TUN capture.
    Off,
    /// Capture through the native desktop System Proxy.
    SystemProxy,
    /// Configure the runtime TUN path after checking platform permission.
    Tun,
}

/// Capture combination observed without normalizing pre-existing state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedCapturePlan {
    /// Both System Proxy and TUN are observed or configured off.
    Off,
    /// Native System Proxy is active while TUN is configured off.
    SystemProxy,
    /// TUN is configured on while native System Proxy is off.
    TunConfigured,
    /// System Proxy and TUN are both present; the state is preserved until an explicit choice.
    Advanced,
    /// One or more required observations are unavailable.
    Unknown,
}

/// Read-only capture state returned by [`TrafficCaptureSession`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficCaptureSnapshot {
    /// System Proxy intent, native readback, and ownership.
    pub system_proxy: Observation<SystemProxySessionSnapshot>,
    /// TUN configuration, permission, and runtime activation fact.
    pub tun: Observation<TunCaptureStatus>,
    /// Combination derived from trustworthy System Proxy and TUN values.
    pub observed_plan: ObservedCapturePlan,
    /// HTTP/Mixed listener available for native proxy capture.
    pub system_proxy_port: Option<u16>,
    /// Whether the current runtime core is available.
    pub core_available: bool,
}

impl TrafficCaptureSnapshot {
    fn from_backend(snapshot: CaptureBackendSnapshot) -> Self {
        let observed_at_ms = now_ms();
        let system_proxy = Observation::record(
            &Observation::Loading,
            snapshot.system_proxy.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let tun = Observation::record(
            &Observation::Loading,
            snapshot.tun.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let observed_plan = observed_capture_plan(&system_proxy, &tun);
        Self {
            system_proxy,
            tun,
            observed_plan,
            system_proxy_port: snapshot.system_proxy_port,
            core_available: snapshot.core_available,
        }
    }
}

/// Result of applying or releasing one capture plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The requested plan was applied; the snapshot remains authoritative for activation facts.
    Applied {
        /// Explicit user plan.
        plan: CapturePlan,
        /// Readback after the operation.
        snapshot: TrafficCaptureSnapshot,
    },
    /// The requested plan already matched current state.
    Unchanged {
        /// Current capture snapshot.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A later step failed and the earlier mutation was restored.
    RolledBack {
        /// Requested plan that did not complete.
        plan: CapturePlan,
        /// Original operation failure.
        failure: String,
        /// Readback after rollback.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A partial operation or failed rollback requires explicit reconciliation.
    ReconcileNeeded {
        /// Requested plan, when the outcome followed an apply operation.
        plan: Option<CapturePlan>,
        /// Combined operation and rollback failure.
        failure: String,
        /// Best-effort readback after the uncertain transition.
        snapshot: TrafficCaptureSnapshot,
    },
}

impl CaptureOutcome {
    /// Returns the latest read-only capture snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &TrafficCaptureSnapshot {
        match self {
            Self::Applied { snapshot, .. }
            | Self::Unchanged { snapshot }
            | Self::RolledBack { snapshot, .. }
            | Self::ReconcileNeeded { snapshot, .. } => snapshot,
        }
    }
}

/// Durable configuration receipt paired with the resulting capture observation.
#[derive(Clone, Debug)]
pub struct ServiceTunOutcome {
    core: Option<crate::CoreApplyOutcome>,
    capture: CaptureOutcome,
    commit_pending: bool,
    recovery_warning: Option<String>,
}

/// Frozen capture observation and the runtime result before native service maintenance.
pub struct ServiceLocalRecoveryOutcome {
    before: TrafficCaptureSnapshot,
    runtime: crate::CoreLocalRecoveryOutcome,
    capture_revision: u64,
}

impl ServiceLocalRecoveryOutcome {
    pub(crate) const fn capture_revision(&self) -> u64 {
        self.capture_revision
    }

    /// Reads the capture facts held before owned native capture was released.
    #[must_use]
    pub const fn before(&self) -> &TrafficCaptureSnapshot {
        &self.before
    }

    /// Reads Local readiness and the scoped resources for a later service handover.
    #[must_use]
    pub const fn runtime(&self) -> &crate::CoreLocalRecoveryOutcome {
        &self.runtime
    }
}

impl ServiceTunOutcome {
    /// Returns a receipt only when business persistence succeeded, including pending finalization.
    #[must_use]
    pub const fn core(&self) -> Option<&crate::CoreApplyOutcome> {
        self.core.as_ref()
    }
    /// Returns capture success, confirmed rollback, or reconciliation needed after an uncertain result.
    #[must_use]
    pub const fn capture(&self) -> &CaptureOutcome {
        &self.capture
    }

    /// Reports an accepted configuration whose native commit still needs confirmation.
    #[must_use]
    pub const fn commit_pending(&self) -> bool {
        self.commit_pending
    }

    /// Reads a capture or cleanup warning that native commit confirmation cannot resolve.
    #[must_use]
    pub fn recovery_warning(&self) -> Option<&str> {
        self.recovery_warning.as_deref()
    }
}

/// Deep module serializing capture plan changes and partial-failure recovery.
#[derive(Clone)]
pub struct TrafficCaptureSession {
    backend: Arc<dyn CaptureBackend>,
    profile: Arc<RwLock<Option<PathBuf>>>,
    operation: Arc<tokio::sync::Mutex<()>>,
    intent_revision: Arc<AtomicU64>,
}

enum CaptureOperation {
    Apply(CapturePlan),
    Reconcile,
    Release,
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

    /// Applies one ordinary capture plan with rollback after partial failure.
    ///
    /// # Errors
    ///
    /// Returns an error before partial mutation, including missing profiles,
    /// unavailable permissions, and external System Proxy ownership conflicts.
    pub async fn apply(&self, plan: CapturePlan) -> Result<CaptureOutcome, TrafficCaptureError> {
        self.note_capture_intent();
        self.complete_admitted(CaptureOperation::Apply(plan)).await
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

    async fn apply_admitted(
        &self,
        plan: CapturePlan,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let mut before = self.snapshot_from_backend().await?;
        if plan_matches(plan, &before) {
            return Ok(CaptureOutcome::Unchanged { snapshot: before });
        }
        if plan == CapturePlan::SystemProxy && before.system_proxy_port.is_none() {
            self.backend.resync_runtime_profile().await?;
            before = self.snapshot_from_backend().await?;
            if plan_matches(plan, &before) {
                return Ok(CaptureOutcome::Unchanged { snapshot: before });
            }
        }
        if plan != CapturePlan::SystemProxy && has_external_system_proxy(&before) {
            return Err(TrafficCaptureError::ExternalSystemProxy);
        }
        match plan {
            CapturePlan::Off => self.apply_off(before).await,
            CapturePlan::SystemProxy => self.apply_system_proxy(before).await,
            CapturePlan::Tun => self.apply_tun(before).await,
        }
    }

    /// Reconciles persistent System Proxy intent without normalizing an
    /// existing System Proxy + TUN advanced combination.
    ///
    /// # Errors
    ///
    /// Returns a backend error when no trustworthy snapshot can be produced.
    pub async fn reconcile(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        self.complete_admitted(CaptureOperation::Reconcile).await
    }

    async fn reconcile_admitted(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        if let Err(failure) = self.backend.reconcile().await {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: None,
                snapshot: self.best_effort_snapshot().await,
                failure: failure.to_string(),
            });
        }
        Ok(CaptureOutcome::Unchanged {
            snapshot: self.snapshot_from_backend().await?,
        })
    }

    /// Releases only native state that still matches ZenClash ownership.
    ///
    /// # Errors
    ///
    /// Returns a backend error when release or readback fails.
    pub async fn release_owned(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        self.complete_admitted(CaptureOperation::Release).await
    }

    async fn release_admitted(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        if let Err(failure) = self.backend.release_owned().await {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: None,
                snapshot: self.best_effort_snapshot().await,
                failure: failure.to_string(),
            });
        }
        Ok(CaptureOutcome::Unchanged {
            snapshot: self.snapshot_from_backend().await?,
        })
    }

    async fn complete_admitted(
        &self,
        operation: CaptureOperation,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        // Cancellation while queuing creates no completion task. Once admitted,
        // completion retains the guard through native writes, readback and rollback.
        let guard = self.operation.clone().lock_owned().await;
        let session = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            match operation {
                CaptureOperation::Apply(plan) => session.apply_admitted(plan).await,
                CaptureOperation::Reconcile => session.reconcile_admitted().await,
                CaptureOperation::Release => session.release_admitted().await,
            }
        })
        .await
        .map_err(|source| task_failure(TrafficCaptureTask::Completion, source))?
    }

    async fn apply_off(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let system_was_owned = system_proxy_is_owned_and_active(&before);
        if system_proxy_has_intent_or_ownership(&before) {
            self.backend.set_system_proxy(false, 0).await?;
        }
        let tun_was_configured = tun_is_configured(&before);
        if tun_was_configured && let Err(failure) = self.backend.set_tun(false).await {
            let rollback = if system_was_owned {
                self.restore_system_proxy(&before).await
            } else {
                Ok(())
            };
            return Ok(self
                .failed_outcome(CapturePlan::Off, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::Off, &before).await
    }

    async fn apply_system_proxy(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let port = before
            .system_proxy_port
            .ok_or_else(|| TrafficCaptureError::MissingSystemProxyPort)?;
        let tun_was_configured = tun_is_configured(&before);
        if tun_was_configured {
            self.backend.set_tun(false).await?;
            let snapshot = self.snapshot_from_backend().await?;
            if snapshot.tun.value().is_none_or(|tun| tun.configured) {
                return Ok(CaptureOutcome::ReconcileNeeded {
                    plan: Some(CapturePlan::SystemProxy),
                    failure: zenclash_i18n::text("traffic_capture.errors.tun_disable_unconfirmed"),
                    snapshot,
                });
            }
        }
        if let Err(failure) = self.backend.set_system_proxy(true, port).await {
            let rollback = if tun_was_configured {
                self.backend.set_tun(true).await
            } else {
                Ok(())
            };
            return Ok(self
                .failed_outcome(CapturePlan::SystemProxy, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::SystemProxy, &before).await
    }

    async fn apply_tun(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        self.backend.ensure_tun_permission().await?;
        let tun_was_configured = tun_is_configured(&before);
        if !tun_was_configured {
            self.backend.set_tun(true).await?;
        }
        if system_proxy_has_intent_or_ownership(&before)
            && let Err(failure) = self.backend.set_system_proxy(false, 0).await
        {
            let rollback = if tun_was_configured {
                Ok(())
            } else {
                self.backend.set_tun(false).await
            };
            return Ok(self
                .failed_outcome(CapturePlan::Tun, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::Tun, &before).await
    }

    async fn restore_system_proxy(
        &self,
        before: &TrafficCaptureSnapshot,
    ) -> Result<(), TrafficCaptureError> {
        let port = before
            .system_proxy_port
            .or_else(|| {
                before
                    .system_proxy
                    .value()
                    .map(|snapshot| snapshot.actual.port)
                    .filter(|port| *port != 0)
            })
            .ok_or(TrafficCaptureError::MissingSystemProxyPort)?;
        self.backend.set_system_proxy(true, port).await
    }

    async fn applied(
        &self,
        plan: CapturePlan,
        before: &TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let snapshot = self.snapshot_from_backend().await?;
        if !plan_matches(plan, &snapshot) {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: Some(plan),
                failure: zenclash_i18n::text("traffic_capture.errors.plan_unconfirmed"),
                snapshot,
            });
        }
        // Match Clash Verge's system-proxy cleanup. TUN teardown belongs to
        // Mihomo; clearing every connection here also disrupts unrelated pools.
        let retiring_system_proxy = system_proxy_is_owned_and_active(before)
            && !system_proxy_is_owned_and_active(&snapshot);
        if retiring_system_proxy && let Err(error) = self.backend.close_owned_connections().await {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: Some(plan),
                failure: zenclash_i18n::text_with(
                    "traffic_capture.errors.connections_cleanup",
                    &[("error", error.to_string())],
                ),
                snapshot,
            });
        }
        Ok(CaptureOutcome::Applied { plan, snapshot })
    }

    async fn failed_outcome(
        &self,
        plan: CapturePlan,
        failure: TrafficCaptureError,
        rollback: Result<(), TrafficCaptureError>,
    ) -> CaptureOutcome {
        let snapshot = self.best_effort_snapshot().await;
        match rollback {
            Ok(()) if failure.is_native_recovery_unconfirmed() => CaptureOutcome::ReconcileNeeded {
                plan: Some(plan),
                failure: failure.to_string(),
                snapshot,
            },
            Ok(()) => CaptureOutcome::RolledBack {
                plan,
                failure: failure.to_string(),
                snapshot,
            },
            Err(rollback) => CaptureOutcome::ReconcileNeeded {
                plan: Some(plan),
                failure: format!("{failure}；回滚失败：{rollback}"),
                snapshot,
            },
        }
    }

    async fn snapshot_from_backend(&self) -> Result<TrafficCaptureSnapshot, TrafficCaptureError> {
        self.backend
            .snapshot()
            .await
            .map(TrafficCaptureSnapshot::from_backend)
    }

    async fn best_effort_snapshot(&self) -> TrafficCaptureSnapshot {
        self.snapshot_from_backend()
            .await
            .unwrap_or_else(|error| failed_snapshot(error.to_string()))
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

fn observed_capture_plan(
    system_proxy: &Observation<SystemProxySessionSnapshot>,
    tun: &Observation<TunCaptureStatus>,
) -> ObservedCapturePlan {
    let (Some(system_proxy), Some(tun)) = (system_proxy.value(), tun.value()) else {
        return ObservedCapturePlan::Unknown;
    };
    match (system_proxy.actual.active(), tun.configured) {
        (false, false) => ObservedCapturePlan::Off,
        (true, false) => ObservedCapturePlan::SystemProxy,
        (false, true) => ObservedCapturePlan::TunConfigured,
        (true, true) => ObservedCapturePlan::Advanced,
    }
}

fn plan_matches(plan: CapturePlan, snapshot: &TrafficCaptureSnapshot) -> bool {
    match plan {
        CapturePlan::Off => {
            snapshot.observed_plan == ObservedCapturePlan::Off
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    !system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Unowned
                })
        }
        CapturePlan::SystemProxy => {
            snapshot.observed_plan == ObservedCapturePlan::SystemProxy
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Owned
                })
        }
        CapturePlan::Tun => snapshot.observed_plan == ObservedCapturePlan::TunConfigured,
    }
}

fn has_external_system_proxy(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership != SystemProxyOwnershipState::Owned
    })
}

fn system_proxy_is_owned_and_active(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

fn system_proxy_has_intent_or_ownership(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.intent_enabled || system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

fn tun_is_configured(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.tun.value().is_some_and(|tun| tun.configured)
}

fn failed_snapshot(failure: String) -> TrafficCaptureSnapshot {
    let failure = crate::OperationalFailure {
        message: failure,
        occurred_at_ms: now_ms(),
    };
    TrafficCaptureSnapshot {
        system_proxy: Observation::Failed {
            failure: failure.clone(),
            recovery: RecoveryAction::ReviewCapture,
        },
        tun: Observation::Failed {
            failure,
            recovery: RecoveryAction::ReviewCapture,
        },
        observed_plan: ObservedCapturePlan::Unknown,
        system_proxy_port: None,
        core_available: false,
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
