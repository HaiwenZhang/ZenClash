// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json.
//! Versioned service health, session intent and privileged-operation admission.
//!
//! `CoreSession` owns core lifecycle transitions. This store retains health and pending
//! actions, projects running-mode facts through its host, and makes read freshness explicit.

mod env;
use crate::runtime::service::ipc::health;
mod owner;
use crate::runtime::service::ipc::probe;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use tokio::sync::Notify;

#[cfg(test)]
pub use env::FakeEnv;
pub use env::{NativeEnv, RunStateEnv, RunStateHost};
pub use health::{PendingAction, RunState, RunStateView, ServiceHealth};
mod recovery;
pub use recovery::{
    OwnerRecoveryPolicy, mark_service_unavailable_after_owner_loss, owner_recovery_policy,
};

pub use owner::{OwnerRecoveryReason, OwnerSample, OwnerStep, OwnerWatch};
pub use probe::{ServiceVersionCheck, ServiceVersionReply, classify_service_version_reply};

use crate::runtime::service::ipc::RunningMode;
use health::StoredService;
use probe::{CurrentServiceProbe, classify_service_health, probe_outcome};

/// Distinguishes silence from a reply that updated Service health with a rejection.
#[derive(Debug)]
pub enum ReadyWaitError {
    /// No readable reply within the attempts budget.
    Unreachable(anyhow::Error),
    /// The Service answered and we rejected the answer; health records why.
    Rejected(anyhow::Error),
}

impl std::fmt::Display for ReadyWaitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(error) | Self::Rejected(error) => write!(formatter, "{error:#}"),
        }
    }
}

/// Service state and revisions share one lock so observations cannot overtake their version bump.
#[derive(Debug, Default)]
struct VersionedService {
    service: StoredService,
    generation: u64,
    observation: u64,
}

impl VersionedService {
    const fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.observation = self.observation.wrapping_add(1);
    }

    const fn next_observation(&mut self) -> u64 {
        self.observation = self.observation.wrapping_add(1);
        self.observation
    }
}

/// Holds the Run State and the effects needed to keep it current.
#[derive(Debug)]
pub struct RunStateStore<E: RunStateEnv> {
    env: Arc<E>,
    service: Mutex<VersionedService>,
    mode: ArcSwap<RunningMode>,
    operation_running: AtomicBool,
    operation_done: Notify,
}

impl<E: RunStateEnv> RunStateStore<E> {
    /// Creates session-owned state with unknown health and no running core.
    pub fn new(env: E) -> Self {
        Self {
            env: Arc::new(env),
            service: Mutex::new(VersionedService::default()),
            mode: ArcSwap::new(Arc::new(RunningMode::NotRunning)),
            operation_running: AtomicBool::new(false),
            operation_done: Notify::new(),
        }
    }

    /// The last known Run State, without waiting and without probing.
    /// It reports when a privileged operation may make the snapshot stale.
    pub fn state(&self) -> RunState {
        let service = self.service.lock().service.clone();
        self.snapshot(service)
    }

    /// Returns one snapshot taken after any operation visible in that snapshot has settled.
    pub async fn settled(&self) -> RunState {
        loop {
            let notified = self.operation_done.notified();
            tokio::pin!(notified);
            // Register before reading; `notify_waiters` leaves no permit for late waiters.
            notified.as_mut().enable();

            let state = self.state();
            if !state.op_in_flight {
                return state;
            }
            notified.await;
        }
    }

    /// Records readable replies; transport failures preserve the last confirmed health.
    ///
    /// # Errors
    /// Returns native probe errors or a readable rejection. Transport failure preserves the last confirmed health.
    pub async fn probe(&self) -> Result<RunState> {
        let reply = self
            .env
            .probe_service_version()
            .await
            .context("service readiness probe failed")?;
        self.record_reply(&reply)
    }

    /// Retries transport failures but stops at the first readable reply, including a rejection.
    ///
    /// # Errors
    /// Returns `Unreachable` after the retry budget or `Rejected` for the first readable incompatible/core-unavailable reply.
    pub async fn await_ready(
        &self,
        attempts: usize,
        interval: Duration,
    ) -> Result<RunState, ReadyWaitError> {
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.env.probe_service_version().await {
                Ok(reply) => return self.record_reply(&reply).map_err(ReadyWaitError::Rejected),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(interval).await;
            }
        }

        Err(ReadyWaitError::Unreachable(last_error.unwrap_or_else(
            || anyhow::anyhow!("service readiness wait was configured with no attempts"),
        )))
    }

    fn record_reply(&self, reply: &ServiceVersionReply) -> Result<RunState> {
        match classify_service_version_reply(reply) {
            ServiceVersionCheck::Ready => {
                self.observe(ServiceHealth::Ready);
                Ok(self.state())
            }
            ServiceVersionCheck::CoreUnavailable(error) => {
                self.observe(ServiceHealth::Unavailable(error.clone()));
                bail!(error)
            }
            ServiceVersionCheck::NeedsReinstall(error) => {
                self.observe(ServiceHealth::VersionMismatch);
                bail!(error)
            }
        }
    }

    /// Detects health from platform evidence and then live IPC; unknown evidence is not absence.
    pub async fn detect_service_health(&self) -> ServiceHealth {
        let has_marker = match self.env.trusted_install_evidence().await {
            Ok(exists) => exists,
            Err(error) => {
                tracing::warn!("failed to inspect trusted service evidence: {error:#}");
                return ServiceHealth::Unavailable(format!("service detection failed: {error:#}"));
            }
        };

        if !has_marker {
            return classify_service_health(CurrentServiceProbe::Missing, false, "");
        }

        match self.env.probe_service_version().await {
            Ok(reply) => match classify_service_version_reply(&reply) {
                ServiceVersionCheck::CoreUnavailable(reason) => ServiceHealth::Unavailable(reason),
                _ => classify_service_health(probe_outcome(&reply), has_marker, ""),
            },
            Err(error) => {
                tracing::warn!("current service IPC is unavailable: {error:#}");
                classify_service_health(
                    CurrentServiceProbe::Unavailable,
                    has_marker,
                    &format!("service detection failed: {error:#}"),
                )
            }
        }
    }

    /// Records detected health unless a newer observation arrived during I/O.
    pub async fn observe_current_health(&self) -> ServiceHealth {
        let reservation = self.reserve_health_observation();
        let health = self.detect_service_health().await;
        self.commit_reserved_observation(health, reservation)
    }

    fn reserve_health_observation(&self) -> u64 {
        self.service.lock().next_observation()
    }

    /// Commits only the latest reserved observation.
    fn commit_reserved_observation(
        &self,
        health: ServiceHealth,
        reservation: u64,
    ) -> ServiceHealth {
        let mut state = self.service.lock();
        if state.observation != reservation {
            let newer = state.service.health.clone();
            drop(state);
            tracing::debug!(
                "discarding a stale service reading of {health:?}; {newer:?} arrived while it ran"
            );
            return newer;
        }
        if state.service.health == health
            && state.service.pending.is_none()
            && !state.service.sidecar_allowed
        {
            return health;
        }
        state.service.observe(health.clone());
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.announce();
        health
    }

    /// Records health and publishes the resulting coherent session state.
    pub fn observe(&self, health: ServiceHealth) {
        let mut state = self.service.lock();
        if state.service.health == health
            && state.service.pending.is_none()
            && !state.service.sidecar_allowed
        {
            // Reassertion is invisible but must invalidate an older health probe.
            state.next_observation();
            return;
        }
        state.service.observe(health);
        state.bump();
        drop(state);
        self.announce();
    }

    /// Records an action and returns the Sidecar allowance it displaced for possible rollback.
    pub fn request_action(&self, action: PendingAction) -> bool {
        let mut state = self.service.lock();
        let displaced = state.service.sidecar_allowed;
        if state.service.pending == Some(action) && !displaced {
            // Reassertion must prevent an older probe from retiring the request.
            state.next_observation();
            return displaced;
        }
        state.service.request(action);
        state.bump();
        drop(state);
        self.announce();
        displaced
    }

    /// Accepts Sidecar when it is the predetermined outcome rather than a fallback.
    pub fn accept_sidecar(&self) {
        let mut state = self.service.lock();
        if state.service.sidecar_allowed && state.service.pending.is_none() {
            // Reassertion must prevent an older probe from clearing the allowance.
            state.next_observation();
            return;
        }
        state.service.allow_sidecar();
        state.bump();
        drop(state);
        self.announce();
    }

    /// Atomically restores an allowance unless Sidecar is already allowed or the Service is ready.
    pub fn restore_sidecar_allowance(&self) -> bool {
        let mut state = self.service.lock();
        if state.service.sidecar_allowed {
            // Reassertion must prevent an older probe from clearing the allowance.
            state.next_observation();
            return false;
        }
        if matches!(state.service.health, ServiceHealth::Ready) {
            // Cached Ready health does not invalidate an in-flight probe.
            return false;
        }
        state.service.allow_sidecar();
        state.bump();
        drop(state);
        self.announce();
        true
    }

    /// Revokes a failed Sidecar allowance without changing Service health.
    pub fn withdraw_sidecar_allowance(&self) -> bool {
        let mut state = self.service.lock();
        if !state.service.sidecar_allowed {
            return false;
        }
        state.service.sidecar_allowed = false;
        state.bump();
        drop(state);
        self.announce();
        true
    }

    /// Accepts Sidecar only while the Service is unusable and no conflicting operation is active.
    ///
    /// # Errors
    /// Returns an error when an operation is in flight or current health and pending intent prohibit Sidecar.
    pub fn allow_sidecar_for_session(&self) -> Result<()> {
        let mut state = self.service.lock();
        if self.operation_running.load(Ordering::Acquire) {
            bail!("service operation already running");
        }
        let permitted = match (state.service.pending, state.service.sidecar_allowed) {
            (Some(PendingAction::Install), _) => true,
            (Some(_), _) | (None, true) => false,
            (None, false) => matches!(
                state.service.health,
                ServiceHealth::NotInstalled
                    | ServiceHealth::VersionMismatch
                    | ServiceHealth::Unavailable(_)
            ),
        };
        if !permitted {
            bail!(
                "sidecar cannot be allowed from service state {:?}",
                state.service
            );
        }
        state.service.allow_sidecar();
        state.bump();
        drop(state);
        self.announce();
        Ok(())
    }

    #[cfg(target_os = "windows")]
    ///
    /// # Errors
    /// Returns an error when privileged work or a pending action prevents fallback.
    pub fn allow_sidecar_after_service_refusal(&self, reason: String) -> Result<()> {
        let mut state = self.service.lock();
        if self.operation_running.load(Ordering::Acquire) || state.service.pending.is_some() {
            bail!("cannot fall back while a service operation is pending");
        }
        // Publish the refusal and allowance together so it never asks the UI for a repair.
        state.service.health = ServiceHealth::Unavailable(reason);
        state.service.allow_sidecar();
        state.bump();
        drop(state);
        self.announce();
        Ok(())
    }

    /// Requests installation only for a confirmed absent Service.
    ///
    /// # Errors
    /// Returns an error when current installation state or in-flight work prohibits requesting install.
    pub fn require_install_for_session(&self) -> Result<()> {
        let mut state = self.service.lock();
        if self.operation_running.load(Ordering::Acquire) {
            bail!("service operation already running");
        }
        if state.service.pending.is_none()
            && !state.service.sidecar_allowed
            && state.service.health == ServiceHealth::NotInstalled
        {
            state.service.request(PendingAction::Install);
            state.bump();
        } else if state.service.pending == Some(PendingAction::Install) {
            // Reassertion must prevent an older probe from clearing the install request.
            state.next_observation();
        }
        drop(state);
        self.announce();
        Ok(())
    }

    /// Runs an operation, recording uninstall success or re-probing after failure.
    ///
    /// # Errors
    /// Returns privileged-helper or readiness errors; the admitted operation may already have modified registration.
    pub async fn perform(&self, action: PendingAction) -> Result<()> {
        // Native authorization may wait for user input; keep it off the async/UI worker.
        let env = Arc::clone(&self.env);
        let outcome = tokio::task::spawn_blocking(move || env.run_privileged(action))
            .await
            .context("privileged service worker did not finish")?;
        match &outcome {
            Ok(()) if matches!(action, PendingAction::Uninstall) => {
                self.observe(ServiceHealth::NotInstalled)
            }
            Ok(()) => {}
            Err(error) => {
                let health = self.observe_current_health().await;
                tracing::warn!(
                    "privileged service action {action:?} did not complete ({error:#}); service is {health:?}"
                );
            }
        }
        outcome
    }

    /// The Running Mode, shared so hot callers such as the tray do not allocate.
    pub fn mode_arc(&self) -> Arc<RunningMode> {
        Arc::clone(&self.mode.load())
    }

    /// Publishes the acknowledged backend after core startup.
    pub fn core_started(&self, mode: RunningMode) {
        self.enter_mode(mode);
    }

    /// Publishes that this session no longer owns a running core.
    pub fn core_stopped(&self) {
        self.enter_mode(RunningMode::NotRunning);
    }

    /// Closes PAC during a start or handover without changing the last running mode.
    /// Every call must be paired with [`Self::core_start_settled`].
    pub fn core_starting(&self) {
        self.env.set_pac_available(false);
    }

    /// Idempotently re-derives PAC availability after a start attempt settles.
    pub fn core_start_settled(&self) {
        let running = !matches!(**self.mode.load(), RunningMode::NotRunning);
        self.env.set_pac_available(running);
    }

    /// Moves to `mode`, closing PAC before the transition and reopening it only afterward.
    fn enter_mode(&self, mode: RunningMode) {
        let running = !matches!(mode, RunningMode::NotRunning);
        if !running {
            self.env.set_pac_available(false);
        }
        self.mode.store(Arc::new(mode));
        if running {
            self.env.set_pac_available(true);
        }
        self.announce();
    }

    /// Claim the privileged-operation slot, releasing it when the guard drops.
    ///
    /// Only one privileged operation may be in flight; [`Self::settled`] waits on this.
    ///
    /// # Errors
    /// Returns an error when another privileged operation already owns the slot.
    pub fn begin_operation(&self) -> Result<OperationGuard<'_, E>> {
        self.claim_operation_slot()?;
        Ok(OperationGuard { store: self })
    }

    pub(crate) fn claim_operation_slot(&self) -> Result<()> {
        let mut state = self.service.lock();
        if self.operation_running.swap(true, Ordering::AcqRel) {
            bail!("service operation already running");
        }
        state.bump();
        drop(state);
        self.announce();
        Ok(())
    }

    pub(crate) fn release_operation_slot(&self) {
        self.operation_running.store(false, Ordering::Release);
        self.operation_done.notify_waiters();
        self.announce();
    }

    #[cfg(test)]
    /// Whether the shared maintenance operation slot is reserved.
    pub fn operation_in_flight(&self) -> bool {
        self.operation_running.load(Ordering::Acquire)
    }

    #[cfg(test)]
    /// Returns the observation and effects adapter owned by this store.
    pub fn env(&self) -> &E {
        &self.env
    }

    #[cfg(test)]
    /// Returns the current published state generation.
    pub fn generation_count(&self) -> u64 {
        self.service.lock().generation
    }

    /// Publishes after dropping the state lock so observers may safely read it back.
    fn announce(&self) {
        self.env.publish(&self.state());
    }

    fn snapshot(&self, service: StoredService) -> RunState {
        RunState {
            health: service.health,
            pending: service.pending,
            sidecar_allowed: service.sidecar_allowed,
            mode: *self.mode.load().as_ref(),
            is_admin: self.env.is_elevated(),
            op_in_flight: self.operation_running.load(Ordering::Acquire),
        }
    }
}

/// Releases the privileged-operation slot and wakes everything waiting in [`RunStateStore::settled`].
#[derive(Debug)]
pub struct OperationGuard<'a, E: RunStateEnv> {
    store: &'a RunStateStore<E>,
}

impl<E: RunStateEnv> Drop for OperationGuard<'_, E> {
    fn drop(&mut self) {
        self.store.operation_running.store(false, Ordering::Release);
        self.store.operation_done.notify_waiters();
        self.store.announce();
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert by panicking"
)]
mod tests;

#[cfg(test)]
mod scene_tests;
