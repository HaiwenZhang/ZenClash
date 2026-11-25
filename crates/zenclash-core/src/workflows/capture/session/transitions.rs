//! Admitted capture-plan transitions, rollback, reconciliation, and release operations.

use super::{
    CaptureOutcome, CapturePlan, TrafficCaptureError, TrafficCaptureSession,
    TrafficCaptureSnapshot, TrafficCaptureTask,
    plans::{
        failed_snapshot, has_external_system_proxy, plan_matches,
        system_proxy_has_intent_or_ownership, system_proxy_is_owned_and_active, tun_is_configured,
    },
    task_failure,
};

enum CaptureOperation {
    Apply(CapturePlan),
    Reconcile,
    Release,
}

impl TrafficCaptureSession {
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

    pub(super) async fn restore_system_proxy(
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

    pub(super) async fn applied(
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

    pub(super) async fn failed_outcome(
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

    pub(super) async fn snapshot_from_backend(
        &self,
    ) -> Result<TrafficCaptureSnapshot, TrafficCaptureError> {
        self.backend
            .snapshot()
            .await
            .map(TrafficCaptureSnapshot::from_backend)
    }

    pub(super) async fn best_effort_snapshot(&self) -> TrafficCaptureSnapshot {
        self.snapshot_from_backend()
            .await
            .unwrap_or_else(|error| failed_snapshot(error.to_string()))
    }
}
