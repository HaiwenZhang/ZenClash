//! Transactional coordination of System Proxy and TUN capture plans.

use crate::core_session::RuntimeConfigurationRevision;
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
mod error;
use error::task_failure;
pub use error::{TrafficCaptureError, TrafficCaptureTask};

use crate::{
    ControlledConfigStore, CoreSession, Observation, RecoveryAction, SystemProxyOwnershipState,
    SystemProxySession, SystemProxySessionSnapshot, TunCaptureStatus,
};

#[cfg(test)]
use crate::CapabilityState;

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
        service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
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
        service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        expected_capture_revision: Option<u64>,
        prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
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
            if result.is_err()
                && let Some(acquired) = acquired
                && !session.backend.owns_service(&acquired)
            {
                acquired.stop().await.map_err(crate::MihomoError::from)?;
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
        service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        expected_capture_revision: Option<u64>,
        prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
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

    fn owns_service(&self, _service: &Arc<crate::service::ServiceSession>) -> bool {
        false
    }
    fn validate_service_tun_request(
        &self,
        _expected_runtime: RuntimeConfigurationRevision,
    ) -> Result<(), crate::CoreSessionError> {
        Ok(())
    }

    fn enable_service_tun(
        &self,
        _service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
        _expected_runtime: RuntimeConfigurationRevision,
        _recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        _prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        crate::core_session::service_tun::ServiceTunRuntimeOutcome,
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
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use crate::{SystemProxyOwnershipState, SystemProxyStatus};

    use super::*;

    struct BlockedWrite {
        entered: tokio::sync::oneshot::Sender<()>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl BlockedWrite {
        fn wait(self) {
            let _ = self.entered.send(());
            let (released, wake) = &*self.release;
            let (_guard, timeout) = wake
                .wait_timeout_while(
                    released.lock().unwrap(),
                    std::time::Duration::from_secs(5),
                    |released| !*released,
                )
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "native write fixture was never released"
            );
        }
    }

    struct ReleaseWrite(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl Drop for ReleaseWrite {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }

    #[tokio::test]
    async fn maintenance_tun_admission_rejects_new_capture_choice_before_handover() {
        let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
        let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
        let revision = capture.capture_revision();
        capture.apply(CapturePlan::Off).await.unwrap();
        let before = backend.operations();
        let result = capture
            .enable_service_tun_with_bundle(
                None,
                crate::core_session::RuntimeConfigurationRevision {
                    binding_generation: 0,
                    configuration_generation: 0,
                },
                None,
                Some(revision),
                None,
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert_eq!(backend.operations(), before);
    }

    #[tokio::test]
    async fn maintenance_proxy_restores_owned_advanced_capture_using_current_listener() {
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
        let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
        let before = capture.snapshot_from_backend().await.unwrap();
        backend.write_proxy(false, 0).unwrap();
        backend.state.lock().unwrap().system_proxy_port = Some(9090);
        capture
            .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
            .await
            .unwrap();
        let after = capture.snapshot_from_backend().await.unwrap();
        assert_eq!(after.observed_plan, ObservedCapturePlan::Advanced);
        assert_eq!(after.system_proxy.value().unwrap().actual.port, 9090);
        assert_eq!(
            backend.operations(),
            ["system-proxy:false", "system-proxy:true"]
        );
    }

    #[tokio::test]
    async fn maintenance_proxy_preserves_external_or_unavailable_current_capture() {
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
        let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
        let before = capture.snapshot_from_backend().await.unwrap();
        backend.state.lock().unwrap().system_proxy.ownership = SystemProxyOwnershipState::Lost;
        assert!(matches!(
            capture
                .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
                .await,
            Err(TrafficCaptureError::ExternalSystemProxy)
        ));
        assert!(backend.operations().is_empty());
        backend.write_proxy(false, 0).unwrap();
        backend.set_core_available(false);
        assert!(
            capture
                .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
                .await
                .is_err()
        );
        assert_eq!(backend.operations(), ["system-proxy:false"]);
    }

    #[tokio::test]
    async fn maintenance_proxy_never_acquires_unowned_or_unknown_previous_capture() {
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Lost, true);
        let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
        let before = capture.snapshot_from_backend().await.unwrap();
        capture
            .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
            .await
            .unwrap();
        assert!(
            capture
                .restore_maintenance_proxy_admitted(
                    &failed_snapshot("unknown".into()),
                    capture.capture_revision()
                )
                .await
                .is_err()
        );
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn maintenance_proxy_rejects_zero_listener_without_native_write() {
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
        let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
        let before = capture.snapshot_from_backend().await.unwrap();
        backend.write_proxy(false, 0).unwrap();
        backend.state.lock().unwrap().system_proxy_port = Some(0);
        assert!(
            capture
                .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
                .await
                .is_err()
        );
        assert_eq!(backend.operations(), ["system-proxy:false"]);
    }

    #[tokio::test]
    async fn maintenance_proxy_rejects_new_capture_intent_without_core_generation_change() {
        let core = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: core.capture_publication_gate(),
            intent_revision: core.capture_intent_revision(),
        };
        let before = capture.snapshot_from_backend().await.unwrap();
        backend.write_proxy(false, 0).unwrap();
        let generation = core.generation();
        let revision = capture.capture_revision();
        capture.apply(CapturePlan::SystemProxy).await.unwrap();
        assert_eq!(core.generation(), generation);
        let count = backend.operations().len();
        assert!(matches!(
            capture
                .restore_maintenance_proxy(
                    &core,
                    before,
                    RuntimeConfigurationRevision {
                        binding_generation: 0,
                        configuration_generation: generation
                    },
                    revision
                )
                .await,
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert_eq!(backend.operations().len(), count);
        assert!(backend.state.lock().unwrap().system_proxy.intent_enabled);
    }

    #[tokio::test]
    async fn maintenance_proxy_rejects_replaced_runtime_without_using_new_version() {
        let core = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: core.capture_publication_gate(),
            intent_revision: core.capture_intent_revision(),
        };
        let before = capture.snapshot_from_backend().await.unwrap();
        core.switch_to_direct(crate::MihomoEndpoint {
            secret: "replacement-maintenance-fixture".into(),
            ..crate::MihomoEndpoint::default()
        })
        .await
        .unwrap();
        assert!(matches!(
            capture
                .restore_maintenance_proxy(
                    &core,
                    before,
                    RuntimeConfigurationRevision {
                        binding_generation: 0,
                        configuration_generation: 0
                    },
                    0
                )
                .await,
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn maintenance_proxy_waiter_cancellation_keeps_shutdown_waiting_for_write() {
        let core = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: core.capture_publication_gate(),
            intent_revision: core.capture_intent_revision(),
        };
        let before = capture.snapshot_from_backend().await.unwrap();
        backend.write_proxy(false, 0).unwrap();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
        *backend.blocked_write.lock().unwrap() = Some(BlockedWrite {
            entered,
            release: release.0.clone(),
        });
        let session = core.clone();
        let task = tokio::spawn(async move {
            capture
                .restore_maintenance_proxy(
                    &session,
                    before,
                    RuntimeConfigurationRevision {
                        binding_generation: 0,
                        configuration_generation: 0,
                    },
                    0,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let session = core.clone();
        let mut shutdown = tokio::spawn(async move { session.shutdown().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut shutdown)
                .await
                .is_err()
        );
        drop(release);
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            backend.operations(),
            ["system-proxy:false", "system-proxy:true"]
        );
        assert!(backend.state.lock().unwrap().tun_configured);
    }

    #[tokio::test]
    async fn cancelled_capture_queue_does_not_start_native_work_after_admission_releases() {
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: session.capture_publication_gate(),
            intent_revision: session.capture_intent_revision(),
        };
        let held = session.capture_publication_gate().lock_owned().await;
        let queued = tokio::spawn(async move { capture.apply(CapturePlan::SystemProxy).await });
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        queued.abort();
        assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
        drop(held);
        tokio::task::yield_now().await;
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn service_recovery_queue_never_adopts_a_new_capture_intent() {
        let core = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: core.capture_publication_gate(),
            intent_revision: core.capture_intent_revision(),
        };
        let held = core.capture_publication_gate().lock_owned().await;
        let queued_capture = capture.clone();
        let session = core.clone();
        let (polled, waiting) = tokio::sync::oneshot::channel();
        let mut polled = Some(polled);
        let recovery = tokio::spawn(async move {
            let store =
                ControlledConfigStore::new(std::env::temp_dir().join("unused-capture-queue-store"));
            let mut future =
                Box::pin(queued_capture.recover_service_to_local(&session, &store, None, 0, 0));
            std::future::poll_fn(|cx| {
                let result = future.as_mut().poll(cx);
                if result.is_pending()
                    && let Some(polled) = polled.take()
                {
                    let _ = polled.send(());
                }
                result
            })
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        let applying = capture.clone();
        let apply = tokio::spawn(async move { applying.apply(CapturePlan::Off).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while capture.capture_revision() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        apply.abort();
        assert!(apply.await.unwrap_err().is_cancelled());
        drop(held);
        assert!(matches!(
            recovery.await.unwrap(),
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert!(backend.operations().is_empty());
        assert_eq!(core.generation(), 0);
    }

    #[tokio::test]
    async fn service_recovery_cancelled_in_capture_queue_performs_no_native_release() {
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: session.capture_publication_gate(),
            intent_revision: session.capture_intent_revision(),
        };
        let binding = session.runtime_descriptor().binding_generation();
        let generation = session.generation();
        let held = session.capture_publication_gate().lock_owned().await;
        let queued = tokio::spawn(async move {
            let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
            capture
                .recover_service_to_local(&session, &store, None, binding, generation)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        queued.abort();
        assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
        drop(held);
        tokio::task::yield_now().await;
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn service_recovery_rechecks_generation_after_capture_queue() {
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: session.capture_publication_gate(),
            intent_revision: session.capture_intent_revision(),
        };
        let binding = session.runtime_descriptor().binding_generation();
        let generation = session.generation();
        let held = session.capture_publication_gate().lock_owned().await;
        let queued_session = session.clone();
        let queued = tokio::spawn(async move {
            let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
            capture
                .recover_service_to_local(&queued_session, &store, None, binding, generation)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        session.invalidate_runtime_observations();
        drop(held);
        assert!(matches!(
            queued.await.unwrap(),
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn service_recovery_rejects_another_core_before_capture_admission() {
        let open = || {
            CoreSession::open(
                crate::CoreKind::Mihomo,
                crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
            )
            .unwrap()
        };
        let session = open();
        let other = open();
        let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: session.capture_publication_gate(),
            intent_revision: session.capture_intent_revision(),
        };
        let _held = session.capture_publication_gate().lock_owned().await;
        let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            capture.recover_service_to_local(
                &other,
                &store,
                None,
                other.runtime_descriptor().binding_generation(),
                other.generation(),
            ),
        )
        .await
        .expect("mismatched owner must not wait on capture");
        assert!(matches!(
            result,
            Err(crate::CoreSessionError::Process(
                crate::MihomoError::StaleBinding
            ))
        ));
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn owner_switch_waits_for_admitted_capture_native_write() {
        exercise_capture_publication(false, false).await;
    }

    #[tokio::test]
    async fn owner_switch_waits_for_admitted_capture_rollback() {
        exercise_capture_publication(false, true).await;
    }

    #[tokio::test]
    async fn cancelled_capture_waiter_keeps_publication_blocked_until_completion() {
        exercise_capture_publication(true, false).await;
    }

    async fn exercise_capture_publication(cancel_waiter: bool, fail_native: bool) {
        use crate::core_session::test_support::OwnedCoreFixture as ChildFixture;
        let first = ChildFixture::new("capture-publication-first").await;
        let second = ChildFixture::new("capture-publication-second").await;
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::from_process(first.process.clone()).unwrap(),
        )
        .unwrap();
        let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, fail_native);
        if fail_native {
            backend.fail_next("system-proxy:true");
        }
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
        *backend.blocked_write.lock().unwrap() = Some(BlockedWrite {
            entered,
            release: release.0.clone(),
        });
        // Real child retirement with a bounded blocking native-side-effect fixture;
        // this does not modify the operating system's proxy settings.
        let capture = TrafficCaptureSession {
            backend: Arc::new(backend.clone()),
            profile: Arc::default(),
            operation: session.capture_publication_gate(),
            intent_revision: session.capture_intent_revision(),
        };
        let apply = tokio::spawn(async move { capture.apply(CapturePlan::SystemProxy).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        if cancel_waiter {
            apply.abort();
        }
        let switch_session = session.clone();
        let replacement = second.process.clone();
        let mut switch =
            tokio::spawn(async move { switch_session.switch_to_process(replacement).await });
        let early = tokio::time::timeout(std::time::Duration::from_millis(100), &mut switch).await;
        let old_running_during_write = first.process.is_running();
        drop(release);
        assert!(
            early.is_err(),
            "owner publication completed while the old native write was paused"
        );
        assert!(
            old_running_during_write,
            "retirement stopped the child targeted by an unfinished native write"
        );
        if cancel_waiter {
            assert!(apply.await.unwrap_err().is_cancelled());
        } else {
            let outcome = apply.await.unwrap().unwrap();
            assert!(if fail_native {
                matches!(outcome, CaptureOutcome::RolledBack { .. })
            } else {
                matches!(outcome, CaptureOutcome::Applied { .. })
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), switch)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!first.process.is_running());
        assert!(second.process.is_running());
        if fail_native {
            assert!(
                backend.state.lock().unwrap().tun_configured,
                "publication escaped before TUN rollback"
            );
        }
        session.shutdown().await.unwrap();
    }

    #[derive(Clone)]
    pub(super) struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
        blocked_write: Arc<Mutex<Option<BlockedWrite>>>,
        blocked_handover: Arc<Mutex<Option<BlockedWrite>>>,
        service_reply:
            Arc<Mutex<Option<crate::core_session::service_tun::ServiceTunRuntimeOutcome>>>,
    }

    struct FakeState {
        system_proxy: SystemProxySessionSnapshot,
        system_proxy_port: Option<u16>,
        resynced_system_proxy_port: Option<u16>,
        tun_configured: bool,
        ignore_tun_writes: bool,
        core_available: bool,
        open_connections: usize,
        permission_error: Option<String>,
        proxy_failure: Option<crate::SystemProxySessionError>,
        permission_requests: usize,
        failures: VecDeque<&'static str>,
        operations: Vec<String>,
    }

    impl FakeBackend {
        #[cfg(all(test, feature = "service-ipc-tests"))]
        pub(super) fn unowned_for_test() -> Self {
            Self::new(false, SystemProxyOwnershipState::Unowned, false)
        }

        fn new(system_active: bool, ownership: SystemProxyOwnershipState, tun: bool) -> Self {
            Self {
                blocked_write: Arc::default(),
                blocked_handover: Arc::default(),
                service_reply: Arc::default(),
                state: Arc::new(Mutex::new(FakeState {
                    system_proxy: SystemProxySessionSnapshot {
                        intent_enabled: system_active,
                        actual: SystemProxyStatus {
                            enabled: system_active,
                            secure_enabled: system_active,
                            port: if system_active { 7890 } else { 0 },
                            secure_port: if system_active { 7890 } else { 0 },
                            ..SystemProxyStatus::default()
                        },
                        ownership,
                    },
                    system_proxy_port: Some(7890),
                    resynced_system_proxy_port: Some(7890),
                    tun_configured: tun,
                    ignore_tun_writes: false,
                    core_available: true,
                    open_connections: 0,
                    permission_error: None,
                    proxy_failure: None,
                    permission_requests: 0,
                    failures: VecDeque::new(),
                    operations: Vec::new(),
                })),
            }
        }

        fn fail_next(&self, operation: &'static str) {
            self.state.lock().unwrap().failures.push_back(operation);
        }

        fn operations(&self) -> Vec<String> {
            self.state.lock().unwrap().operations.clone()
        }

        fn set_core_available(&self, available: bool) {
            self.state.lock().unwrap().core_available = available;
        }

        fn permission_requests(&self) -> usize {
            self.state.lock().unwrap().permission_requests
        }

        fn should_fail(state: &mut FakeState, operation: &str) -> Result<(), TrafficCaptureError> {
            if state
                .failures
                .front()
                .is_some_and(|next| *next == operation)
            {
                state.failures.pop_front();
                Err(TrafficCaptureError::Controller(crate::MihomoError::Api {
                    status: 503,
                    message: format!("{operation} failed"),
                }))
            } else {
                Ok(())
            }
        }

        fn write_proxy(&self, enabled: bool, port: u16) -> Result<(), TrafficCaptureError> {
            let mut state = self.state.lock().unwrap();
            let operation = format!("system-proxy:{enabled}");
            state.operations.push(operation.clone());
            if let Some(error) = state.proxy_failure.take() {
                return Err(error.into());
            }
            Self::should_fail(&mut state, &operation)?;
            state.system_proxy.intent_enabled = enabled;
            state.system_proxy.actual.enabled = enabled;
            state.system_proxy.actual.secure_enabled = enabled;
            state.system_proxy.actual.port = if enabled { port } else { 0 };
            state.system_proxy.actual.secure_port = if enabled { port } else { 0 };
            state.system_proxy.ownership = if enabled {
                SystemProxyOwnershipState::Owned
            } else {
                SystemProxyOwnershipState::Unowned
            };
            Ok(())
        }
    }

    impl CaptureBackend for FakeBackend {
        fn enable_service_tun(
            &self,
            _service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
            _expected_runtime: RuntimeConfigurationRevision,
            _recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
            _prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            crate::core_session::service_tun::ServiceTunRuntimeOutcome,
                            crate::CoreSessionError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                let blocked = self.blocked_handover.lock().unwrap().take();
                if let Some(blocked) = blocked {
                    tokio::task::spawn_blocking(move || blocked.wait())
                        .await
                        .unwrap();
                }
                self.state
                    .lock()
                    .unwrap()
                    .operations
                    .push("service-handover".into());
                Ok(self.service_reply.lock().unwrap().take().unwrap_or(
                    crate::core_session::service_tun::ServiceTunRuntimeOutcome {
                        saved: Some(crate::CoreApplyOutcome {
                            kind: crate::CoreApplyKind::Patched,
                            generation: 1,
                        }),
                        commit_pending: false,
                        recovery_warning: None,
                        failure: None,
                        restored: false,
                    },
                ))
            })
        }

        fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state
                    .operations
                    .iter()
                    .any(|operation| operation == "service-handover")
                {
                    Self::should_fail(&mut state, "capture-readback")?;
                }
                Ok(CaptureBackendSnapshot {
                    system_proxy: Ok(state.system_proxy.clone()),
                    tun: Ok(TunCaptureStatus {
                        requested: state.tun_configured,
                        configured: state.tun_configured,
                        permission: CapabilityState::Active,
                        runtime: crate::TunRuntimeObservation {
                            device_name: Some("test-tun".into()),
                            device: if state.tun_configured {
                                CapabilityState::Active
                            } else {
                                CapabilityState::Inactive
                            },
                            route: if state.tun_configured {
                                CapabilityState::Unknown
                            } else {
                                CapabilityState::Inactive
                            },
                            detail: "test observation".into(),
                        },
                        observed: if state.tun_configured {
                            CapabilityState::Unknown
                        } else {
                            CapabilityState::Inactive
                        },
                    }),
                    system_proxy_port: state.system_proxy_port,
                    core_available: state.core_available,
                })
            })
        }

        fn resync_runtime_profile(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.operations.push("resync-runtime-profile".into());
                state.system_proxy_port = state.resynced_system_proxy_port;
                Ok(())
            })
        }

        fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let blocked = self.blocked_write.lock().unwrap().take();
                if let Some(blocked) = blocked {
                    let backend = self.clone();
                    return tokio::task::spawn_blocking(move || {
                        blocked.wait();
                        backend.write_proxy(enabled, port)
                    })
                    .await
                    .unwrap();
                }
                self.write_proxy(enabled, port)
            })
        }

        fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let operation = format!("tun:{enabled}");
                state.operations.push(operation.clone());
                Self::should_fail(&mut state, &operation)?;
                if !state.ignore_tun_writes {
                    state.tun_configured = enabled;
                }
                Ok(())
            })
        }

        fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.permission_requests += 1;
                state.permission_error.clone().map_or(Ok(()), |message| {
                    Err(TrafficCaptureError::Controller(
                        crate::MihomoError::InvalidInput(message),
                    ))
                })
            })
        }

        fn close_owned_connections(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.operations.push("close-connections".into());
                Self::should_fail(&mut state, "close-connections")?;
                state.open_connections = 0;
                Ok(())
            })
        }

        fn reconcile(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.operations.push("reconcile".into());
                if state.system_proxy.intent_enabled && !state.core_available {
                    if state.system_proxy.ownership == SystemProxyOwnershipState::Owned {
                        state.system_proxy.actual = SystemProxyStatus::default();
                        state.system_proxy.ownership = SystemProxyOwnershipState::Unowned;
                    }
                } else if state.system_proxy.intent_enabled
                    && !state.system_proxy.actual.active()
                    && state.system_proxy.ownership == SystemProxyOwnershipState::Unowned
                {
                    state.system_proxy.actual = SystemProxyStatus {
                        enabled: true,
                        secure_enabled: true,
                        port: 7890,
                        secure_port: 7890,
                        ..SystemProxyStatus::default()
                    };
                    state.system_proxy.ownership = SystemProxyOwnershipState::Owned;
                }
                Ok(())
            })
        }

        fn release_owned(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.operations.push("release-owned".into());
                Self::should_fail(&mut state, "release-owned")?;
                if state.system_proxy.ownership == SystemProxyOwnershipState::Owned {
                    state.system_proxy.actual = SystemProxyStatus::default();
                }
                state.system_proxy.ownership = SystemProxyOwnershipState::Unowned;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn service_handover_saved_unknown_retains_receipt_without_restoring_proxy() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        *backend.service_reply.lock().unwrap() =
            Some(crate::core_session::service_tun::ServiceTunRuntimeOutcome {
                saved: Some(crate::CoreApplyOutcome {
                    kind: crate::CoreApplyKind::Patched,
                    generation: 7,
                }),
                commit_pending: true,
                recovery_warning: None,
                failure: Some(crate::MihomoError::RuntimeOutcomeUnknown.into()),
                restored: false,
            });
        let capture = TrafficCaptureSession::with_backend(backend.clone());
        let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
        assert_eq!(outcome.core().unwrap().generation, 7);
        assert!(outcome.commit_pending());
        assert!(outcome.recovery_warning().is_none());
        assert!(matches!(
            outcome.capture(),
            CaptureOutcome::ReconcileNeeded { .. }
        ));
        assert_eq!(
            backend.operations(),
            ["system-proxy:false", "service-handover"]
        );
    }

    #[tokio::test]
    async fn service_handover_committed_capture_readback_failure_is_not_pending_commit() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.fail_next("capture-readback");
        let capture = TrafficCaptureSession::with_backend(backend);
        let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
        assert!(outcome.core().is_some());
        assert!(!outcome.commit_pending());
        let warning = TrafficCaptureError::Controller(crate::MihomoError::Api {
            status: 503,
            message: "capture-readback failed".into(),
        })
        .to_string();
        assert_eq!(outcome.recovery_warning(), Some(warning.as_str()));
        assert!(matches!(
            outcome.capture(),
            CaptureOutcome::ReconcileNeeded { .. }
        ));
    }

    #[tokio::test]
    async fn service_handover_committed_cleanup_failure_keeps_only_recovery_warning() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
        *backend.service_reply.lock().unwrap() =
            Some(crate::core_session::service_tun::ServiceTunRuntimeOutcome {
                saved: Some(crate::CoreApplyOutcome {
                    kind: crate::CoreApplyKind::Patched,
                    generation: 7,
                }),
                commit_pending: false,
                recovery_warning: Some(warning.clone()),
                failure: Some(crate::CoreSessionError::PreviousCoreCleanupUnconfirmed),
                restored: false,
            });
        let capture = TrafficCaptureSession::with_backend(backend);
        let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
        assert_eq!(outcome.core().unwrap().generation, 7);
        assert!(!outcome.commit_pending());
        assert_eq!(outcome.recovery_warning(), Some(warning.as_str()));
    }

    #[tokio::test]
    async fn service_handover_unsaved_confirmed_restore_restores_owned_proxy() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        *backend.service_reply.lock().unwrap() =
            Some(crate::core_session::service_tun::ServiceTunRuntimeOutcome {
                saved: None,
                commit_pending: false,
                recovery_warning: None,
                failure: Some(
                    crate::MihomoError::Process("injected preparation failure".into()).into(),
                ),
                restored: true,
            });
        let capture = TrafficCaptureSession::with_backend(backend.clone());
        let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
        assert!(outcome.core().is_none());
        assert!(matches!(
            outcome.capture(),
            CaptureOutcome::RolledBack { .. }
        ));
        assert_eq!(
            backend.operations(),
            [
                "system-proxy:false",
                "service-handover",
                "system-proxy:true"
            ]
        );
    }

    #[tokio::test]
    async fn service_handover_waiter_cancel_keeps_shutdown_waiting_for_completion() {
        use crate::core_session::test_support::OwnedCoreFixture as ChildFixture;
        let child = ChildFixture::new("service-capture-shutdown").await;
        let core = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::from_process(child.process.clone()).unwrap(),
        )
        .unwrap();
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
        *backend.blocked_handover.lock().unwrap() = Some(BlockedWrite {
            entered,
            release: release.0.clone(),
        });
        let capture = TrafficCaptureSession {
            backend: backend.clone(),
            profile: Arc::default(),
            operation: core.capture_publication_gate(),
            intent_revision: core.capture_intent_revision(),
        };
        let waiter = tokio::spawn(async move { capture.enable_service_tun(None, 0, 0).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        let closing = core.clone();
        let mut shutdown = tokio::spawn(async move { closing.shutdown().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut shutdown)
                .await
                .is_err()
        );
        assert!(child.process.is_running());
        drop(release);
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(backend.operations(), ["service-handover"]);
        assert!(!child.process.is_running());
    }

    #[tokio::test]
    async fn reconcile_preserves_an_advanced_combination() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            true,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.reconcile().await.unwrap();

        assert_eq!(
            outcome.snapshot().observed_plan,
            ObservedCapturePlan::Advanced
        );
        assert_eq!(backend.operations(), ["reconcile"]);
    }

    #[tokio::test]
    async fn a_tun_disable_not_confirmed_by_readback_cannot_enable_system_proxy() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.state.lock().unwrap().ignore_tun_writes = true;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(outcome, CaptureOutcome::ReconcileNeeded { .. }));
        assert!(outcome.snapshot().tun.value().unwrap().configured);
        assert!(!backend.state.lock().unwrap().system_proxy.actual.active());
    }

    #[tokio::test]
    async fn connection_cleanup_failure_does_not_restore_disabled_capture() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            true,
        ));
        backend.state.lock().unwrap().open_connections = 1;
        backend.fail_next("close-connections");
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::Off).await.unwrap();

        assert!(matches!(outcome, CaptureOutcome::ReconcileNeeded { .. }));
        assert_eq!(outcome.snapshot().observed_plan, ObservedCapturePlan::Off);
        assert_eq!(backend.state.lock().unwrap().open_connections, 1);
        assert_eq!(
            backend.operations(),
            ["system-proxy:false", "tun:false", "close-connections"]
        );
    }

    #[tokio::test]
    async fn connection_cleanup_never_contacts_an_external_controller() {
        let client =
            crate::MihomoClient::new(crate::MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
        let core_session = CoreSession::open(crate::CoreKind::Mihomo, client).unwrap();
        let backend = ProductionCaptureBackend::new(
            core_session,
            ControlledConfigStore::new(
                std::env::temp_dir().join("zenclash-external-connection-cleanup"),
            ),
            None,
            Arc::default(),
        );

        // An attempted request would fail against this unreachable external controller.
        backend.close_owned_connections().await.unwrap();
    }

    #[tokio::test]
    async fn turning_capture_off_closes_existing_connections_without_stopping_core() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        backend.state.lock().unwrap().open_connections = 1;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::Off).await.unwrap();

        assert_eq!(outcome.snapshot().observed_plan, ObservedCapturePlan::Off);
        let state = backend.state.lock().unwrap();
        assert_eq!(state.open_connections, 0);
        assert!(state.core_available);
    }

    #[tokio::test]
    async fn switching_from_tun_to_system_proxy_then_off_releases_both_capture_modes() {
        // Native capture is simulated so this regression cannot change host routing.
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.state.lock().unwrap().open_connections = 1;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let proxy = session.apply(CapturePlan::SystemProxy).await.unwrap();
        assert_eq!(
            proxy.snapshot().observed_plan,
            ObservedCapturePlan::SystemProxy
        );
        assert!(!proxy.snapshot().tun.value().unwrap().configured);
        assert_eq!(backend.state.lock().unwrap().open_connections, 1);

        backend.state.lock().unwrap().open_connections = 1;
        let off = session.apply(CapturePlan::Off).await.unwrap();
        assert_eq!(off.snapshot().observed_plan, ObservedCapturePlan::Off);
        let system_proxy = off.snapshot().system_proxy.value().unwrap();
        assert!(!system_proxy.intent_enabled);
        assert!(!system_proxy.actual.active());
        assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
        assert!(!off.snapshot().tun.value().unwrap().configured);
        assert_eq!(backend.state.lock().unwrap().open_connections, 0);
    }

    #[tokio::test]
    async fn tun_off_then_on_does_not_clear_unrelated_connections() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.state.lock().unwrap().open_connections = 1;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let off = session.apply(CapturePlan::Off).await.unwrap();
        assert_eq!(off.snapshot().observed_plan, ObservedCapturePlan::Off);
        let on = session.apply(CapturePlan::Tun).await.unwrap();
        assert_eq!(
            on.snapshot().observed_plan,
            ObservedCapturePlan::TunConfigured
        );
        let state = backend.state.lock().unwrap();
        assert_eq!(state.open_connections, 1);
        assert!(state.core_available);
    }

    #[tokio::test]
    async fn switching_from_system_proxy_to_tun_closes_old_connections_and_preserves_core() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        backend.state.lock().unwrap().open_connections = 1;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::Tun).await.unwrap();

        assert_eq!(
            outcome.snapshot().observed_plan,
            ObservedCapturePlan::TunConfigured
        );
        assert!(
            !outcome
                .snapshot()
                .system_proxy
                .value()
                .unwrap()
                .actual
                .active()
        );
        let state = backend.state.lock().unwrap();
        assert_eq!(state.open_connections, 0);
        assert!(state.core_available);
    }

    #[tokio::test]
    async fn system_proxy_failure_restores_the_previous_tun_state() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.fail_next("system-proxy:true");
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(outcome, CaptureOutcome::RolledBack { .. }));
        assert_eq!(
            backend.operations(),
            ["tun:false", "system-proxy:true", "tun:true"]
        );
        assert_eq!(
            outcome.snapshot().observed_plan,
            ObservedCapturePlan::TunConfigured
        );
    }

    #[tokio::test]
    async fn system_proxy_resynchronizes_the_runtime_when_the_initial_port_is_missing() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        backend.state.lock().unwrap().system_proxy_port = None;
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(
            outcome,
            CaptureOutcome::Applied {
                plan: CapturePlan::SystemProxy,
                ..
            }
        ));
        assert_eq!(
            backend.operations(),
            ["resync-runtime-profile", "system-proxy:true"]
        );
    }

    #[tokio::test]
    async fn system_proxy_keeps_the_safe_error_when_resynchronization_has_no_port() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        {
            let mut state = backend.state.lock().unwrap();
            state.system_proxy_port = None;
            state.resynced_system_proxy_port = None;
        }
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let error = session
            .apply(CapturePlan::SystemProxy)
            .await
            .expect_err("a profile without HTTP/Mixed listeners remains unsafe");

        assert!(error.to_string().contains(&zenclash_i18n::text(
            "traffic_capture.errors.missing_system_proxy_port"
        )));
        assert_eq!(backend.operations(), ["resync-runtime-profile"]);
    }

    #[tokio::test]
    async fn failed_rollback_exposes_reconcile_needed() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.fail_next("system-proxy:true");
        backend.fail_next("tun:true");
        let session = TrafficCaptureSession::with_backend(backend);

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(
            outcome,
            CaptureOutcome::ReconcileNeeded {
                plan: Some(CapturePlan::SystemProxy),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn external_system_proxy_is_not_overwritten_by_off_or_tun() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Lost,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let result = session.apply(CapturePlan::Off).await;

        assert!(matches!(
            result,
            Err(TrafficCaptureError::ExternalSystemProxy)
        ));
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn rejected_permission_performs_no_capture_write() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        backend.state.lock().unwrap().permission_error = Some("permission rejected".into());
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::Tun).await;

        assert!(matches!(
            outcome,
            Err(TrafficCaptureError::Controller(crate::MihomoError::InvalidInput(message)))
                if message == "permission rejected"
        ));
        assert_eq!(backend.permission_requests(), 1);
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn proxy_io_failure_survives_capture_and_core_error_conversion() {
        use std::{error::Error, io};
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        backend.state.lock().unwrap().proxy_failure = Some(crate::SystemProxySessionError::Native(
            crate::SystemProxyError::PlatformIo {
                backend: crate::SystemProxyBackendKind::MacOs,
                operation: "fixture_native_write",
                source: io::Error::new(io::ErrorKind::PermissionDenied, "fixture write denied"),
            }
            .into(),
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());
        let error = session.apply(CapturePlan::Off).await.unwrap_err();
        assert!(
            matches!(&error, TrafficCaptureError::SystemProxy(crate::SystemProxySessionError::Native(
            crate::MihomoError::SystemProxy(error)))
            if matches!(error.as_ref(), crate::SystemProxyError::PlatformIo { operation: "fixture_native_write", .. }))
        );
        let core_error = crate::CoreSessionError::from(error);
        assert!(matches!(&core_error, crate::CoreSessionError::Capture(_)));
        assert_eq!(
            core_error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(backend.operations(), ["system-proxy:false"]);
    }

    #[tokio::test]
    async fn native_recovery_uncertainty_requires_reconciliation_after_tun_rollback() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.state.lock().unwrap().proxy_failure =
            Some(crate::SystemProxySessionError::Transaction(Box::new(
                crate::SystemProxyError::RecoveryUnconfirmed {
                    cause: Box::new(crate::SystemProxyTransactionCause::Preferences(
                        crate::AppPreferencesError::ConcurrentModification,
                    )),
                    recovery: Box::new(crate::SystemProxyError::VerificationFailed.into()),
                    release_failure: None,
                },
            )));
        let session = TrafficCaptureSession::with_backend(backend.clone());
        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();
        assert!(matches!(
            outcome,
            CaptureOutcome::ReconcileNeeded {
                plan: Some(CapturePlan::SystemProxy),
                ..
            }
        ));
        assert!(backend.state.lock().unwrap().tun_configured);
        assert_eq!(
            backend.operations(),
            ["tun:false", "system-proxy:true", "tun:true"]
        );
    }

    #[tokio::test]
    async fn confirmed_native_recovery_can_report_capture_rollback() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.state.lock().unwrap().proxy_failure =
            Some(crate::SystemProxySessionError::Transaction(Box::new(
                crate::SystemProxyError::RecoveryConfirmed {
                    cause: Box::new(crate::SystemProxyTransactionCause::Preferences(
                        crate::AppPreferencesError::ConcurrentModification,
                    )),
                },
            )));
        let session = TrafficCaptureSession::with_backend(backend.clone());
        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();
        assert!(matches!(
            outcome,
            CaptureOutcome::RolledBack {
                plan: CapturePlan::SystemProxy,
                ..
            }
        ));
        assert!(backend.state.lock().unwrap().tun_configured);
        assert_eq!(
            backend.operations(),
            ["tun:false", "system-proxy:true", "tun:true"]
        );
    }

    #[tokio::test]
    async fn permission_prompt_is_reachable_only_from_an_explicit_tun_plan() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        session.reconcile().await.unwrap();
        session.release_owned().await.unwrap();
        session.apply(CapturePlan::SystemProxy).await.unwrap();
        session.apply(CapturePlan::Off).await.unwrap();

        assert_eq!(backend.permission_requests(), 0);
        session.apply(CapturePlan::Tun).await.unwrap();
        assert_eq!(backend.permission_requests(), 1);
    }

    #[tokio::test]
    async fn normal_exit_releases_owned_system_proxy_without_changing_intent() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.release_owned().await.unwrap();

        let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
        assert!(system_proxy.intent_enabled);
        assert!(!system_proxy.actual.active());
        assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
        assert_eq!(backend.operations(), ["release-owned"]);
    }

    #[tokio::test]
    async fn exit_does_not_overwrite_an_external_system_proxy_replacement() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Lost,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.release_owned().await.unwrap();

        let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
        assert!(system_proxy.actual.active());
        assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
        assert_eq!(backend.operations(), ["release-owned"]);
    }

    #[tokio::test]
    async fn core_crash_releases_owned_proxy_and_restart_restores_persistent_intent() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        backend.set_core_available(false);
        let crashed = session.reconcile().await.unwrap();
        let crashed_proxy = crashed.snapshot().system_proxy.value().unwrap();
        assert!(crashed_proxy.intent_enabled);
        assert!(!crashed_proxy.actual.active());
        assert_eq!(crashed_proxy.ownership, SystemProxyOwnershipState::Unowned);

        backend.set_core_available(true);
        let restarted = session.reconcile().await.unwrap();
        let restarted_proxy = restarted.snapshot().system_proxy.value().unwrap();
        assert!(restarted_proxy.actual.active());
        assert_eq!(restarted_proxy.ownership, SystemProxyOwnershipState::Owned);
        assert_eq!(backend.operations(), ["reconcile", "reconcile"]);
    }
}
