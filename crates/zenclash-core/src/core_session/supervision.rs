//! Bounded observation and recovery of the original session-owned runtime.

use super::*;

const CORE_SUPERVISOR_INTERVAL: Duration = Duration::from_millis(250);
const CORE_RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_CORE_RECOVERY_ATTEMPTS: u32 = 3;

#[derive(Clone, Copy)]
struct CoreRecoveryPolicy {
    interval: Duration,
    retry_delay: Duration,
    ready_timeout: Duration,
    max_attempts: u32,
}

impl Default for CoreRecoveryPolicy {
    fn default() -> Self {
        Self {
            interval: CORE_SUPERVISOR_INTERVAL,
            retry_delay: CORE_RECOVERY_RETRY_DELAY,
            ready_timeout: CORE_READY_TIMEOUT,
            max_attempts: MAX_CORE_RECOVERY_ATTEMPTS,
        }
    }
}

impl CoreSession {
    /// Starts one bounded unexpected-exit supervisor for a managed core.
    ///
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor(&self, runtime: &Handle) -> bool {
        self.start_supervisor_with_policy(runtime, CoreRecoveryPolicy::default(), None)
    }

    /// Starts managed-core supervision with capture release and restoration hooks.
    ///
    /// On an unexpected exit, owned native capture is released before restart;
    /// after controller readiness succeeds, persistent intent is reconciled.
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor_with_capture(
        &self,
        runtime: &Handle,
        capture: TrafficCaptureSession,
    ) -> bool {
        self.start_supervisor_with_policy(
            runtime,
            CoreRecoveryPolicy::default(),
            Some(Arc::new(capture)),
        )
    }

    fn start_supervisor_with_policy(
        &self,
        runtime: &Handle,
        policy: CoreRecoveryPolicy,
        capture: Option<Arc<dyn CoreRecoveryCapture>>,
    ) -> bool {
        if !self.is_managed() || self.supervisor_started.swap(true, Ordering::AcqRel) {
            return false;
        }
        let session = self.clone();
        runtime.spawn(async move { supervise_managed_core(session, policy, capture).await });
        true
    }
}

async fn supervise_managed_core(
    session: CoreSession,
    policy: CoreRecoveryPolicy,
    capture: Option<Arc<dyn CoreRecoveryCapture>>,
) {
    let mut owner_watch = crate::service::runstate::OwnerWatch::new();
    let mut watched_binding = None;
    let mut service_capture_released = false;
    loop {
        if session.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        let client = match session.client.pin_binding() {
            Ok(client) => client,
            Err(_) => continue,
        };
        let observation = match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                tokio::task::spawn_blocking(move || {
                    let snapshot = process.snapshot();
                    (snapshot.running, snapshot.exit_reason)
                })
                .await
                .ok()
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => {
                use crate::service::runstate::{OwnerStep, OwnerWatch};
                let binding = client.runtime_descriptor().binding_generation();
                if watched_binding != Some(binding) {
                    watched_binding = Some(binding);
                    owner_watch = OwnerWatch::new();
                    service_capture_released = false;
                }
                let lifecycle = session.lifecycle.read().clone();
                if lifecycle.stop_requested
                    || session.network_suspended.load(Ordering::Acquire)
                    || !runtime.observation_allowed()
                {
                    session.reconcile_run_state();
                    tokio::time::sleep(policy.interval).await;
                    continue;
                }
                let mut step = owner_watch.observe(runtime.client.owner_sample().await);
                if matches!(step, OwnerStep::VerifyTransport) {
                    let available = client.version().await.is_ok();
                    step = owner_watch.resolve_transport(available);
                }
                if client.ensure_binding_current().is_err() || !runtime.observation_allowed() {
                    continue;
                }
                match step {
                    OwnerStep::Continue => {
                        if let Some(status) = runtime.client.snapshot() {
                            let mut lifecycle = session.lifecycle.write();
                            if !lifecycle.stop_requested {
                                lifecycle.phase = if status.core_pid.is_some() {
                                    CoreLifecyclePhase::Stable
                                } else {
                                    CoreLifecyclePhase::Recovering
                                };
                                lifecycle.exit_reason = status.last_core_exit_reason;
                            }
                        }
                    }
                    OwnerStep::Recover(reason) => {
                        {
                            let mut lifecycle = session.lifecycle.write();
                            if !lifecycle.stop_requested {
                                lifecycle.phase = CoreLifecyclePhase::Unknown;
                                lifecycle.last_error =
                                    Some(format!("Service owner recovery required: {reason:?}"));
                            }
                        }
                        crate::service::runstate::mark_service_unavailable_after_owner_loss(
                            &session.run_state.store,
                            reason,
                        );
                        session.reconcile_run_state();
                        let recovery = crate::service::runstate::owner_recovery_policy(
                            reason,
                            cfg!(target_os = "macos"),
                        );
                        if recovery.reset_system_proxy
                            && !service_capture_released
                            && let Some(capture) = capture.as_ref()
                        {
                            match capture.release_owned().await {
                                Ok(()) => service_capture_released = true,
                                Err(error) => {
                                    tracing::warn!(%error, "could not release capture after lost service ownership")
                                }
                            }
                        }
                    }
                    OwnerStep::VerifyTransport => {
                        unreachable!("transport check is resolved before dispatch")
                    }
                }
                session.reconcile_run_state();
                // Native Service owns crash recovery. A displaced app must never
                // run an automatic Start that takes ownership back from another session.
                tokio::time::sleep(policy.interval).await;
                continue;
            }
            None => {
                tokio::time::sleep(policy.interval).await;
                continue;
            }
        };
        if client.ensure_binding_current().is_err() {
            continue;
        }
        let decision = supervisor_observation(
            &mut session.lifecycle.write(),
            observation,
            session.network_suspended.load(Ordering::Acquire),
            session.shutdown_requested.load(Ordering::Acquire),
            session.kind,
            policy.max_attempts,
        );
        session.reconcile_run_state();
        let Some(decision) = decision else {
            tokio::time::sleep(policy.interval).await;
            continue;
        };
        if decision.retry_budget_exhausted {
            tokio::time::sleep(policy.interval).await;
            continue;
        }
        if decision.first_exit_observation
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.release_owned().await
        {
            tracing::warn!(%error, "failed to release owned capture after managed-core exit");
        }

        if session.lifecycle.read().recovery_attempts > 0 {
            tokio::time::sleep(policy.retry_delay).await;
            if session.shutdown_requested.load(Ordering::Acquire) {
                return;
            }
        }

        let recovered = recover_managed_core(&session, policy).await;
        if recovered
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.reconcile().await
        {
            tracing::warn!(%error, "failed to reconcile capture after managed-core recovery");
        }
        if recovered || session.shutdown_requested.load(Ordering::Acquire) {
            tokio::time::sleep(policy.interval).await;
        }
    }
}

pub(super) struct ManagedExitRecoveryDecision {
    first_exit_observation: bool,
    retry_budget_exhausted: bool,
}

pub(super) fn supervisor_observation(
    lifecycle: &mut CoreLifecycleSnapshot,
    observation: Option<(bool, Option<String>)>,
    network_suspended: bool,
    shutdown: bool,
    kind: CoreKind,
    max_attempts: u32,
) -> Option<ManagedExitRecoveryDecision> {
    // Observation cannot revoke an explicit lifecycle intention. Status failures
    // describe missing evidence, rather than permission to recover a stopped core.
    if shutdown
        || network_suspended
        || lifecycle.stop_requested
        || matches!(
            lifecycle.phase,
            CoreLifecyclePhase::Stopped
                | CoreLifecyclePhase::NetworkSuspended
                | CoreLifecyclePhase::ShuttingDown
        )
    {
        return None;
    }
    let Some((running, exit_reason)) = observation else {
        lifecycle.phase = CoreLifecyclePhase::Unknown;
        return None;
    };
    if running {
        if lifecycle.phase == CoreLifecyclePhase::Unknown {
            lifecycle.phase = CoreLifecyclePhase::Stable;
        }
        return None;
    }
    if network_suspended || lifecycle.phase == CoreLifecyclePhase::Stopped {
        return None;
    }
    let new_exit = lifecycle.phase == CoreLifecyclePhase::Stable;
    if new_exit {
        lifecycle.recovery_attempts = 0;
        lifecycle.exit_reason = exit_reason.or_else(|| Some(format!("{kind} 托管进程意外退出")));
        lifecycle.last_error = None;
    }
    let exhausted = lifecycle.recovery_attempts >= max_attempts;
    lifecycle.phase = if exhausted {
        CoreLifecyclePhase::Failed
    } else {
        CoreLifecyclePhase::Recovering
    };
    Some(ManagedExitRecoveryDecision {
        first_exit_observation: new_exit,
        retry_budget_exhausted: exhausted,
    })
}

async fn recover_managed_core(session: &CoreSession, policy: CoreRecoveryPolicy) -> bool {
    let client = match session.client.pin_binding() {
        Ok(client) => client,
        Err(error) => {
            session.lifecycle.write().last_error = Some(error.to_string());
            return false;
        }
    };
    let lease = match session.acquire_runtime_write_lease(&client).await {
        Ok(lease) => lease,
        Err(error) => {
            session.lifecycle.write().last_error = Some(error.to_string());
            return false;
        }
    };
    let _transition = session.transition.lock().await;
    if let Err(error) = client.ensure_binding_current() {
        session.lifecycle.write().last_error = Some(error.to_string());
        return false;
    }
    if session.shutdown_requested.load(Ordering::Acquire)
        || session.network_suspended.load(Ordering::Acquire)
        || session.lifecycle.read().stop_requested
    {
        return false;
    }
    let _mutation = match client.lock_runtime_binding().await {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if client.owned_core().is_none() {
        return false;
    }
    let _availability_pause = session.pause_pac_availability();
    if session.snapshot().running == Some(true) {
        session.lifecycle.write().phase = CoreLifecyclePhase::Stable;
        return true;
    }
    let attempt = {
        let mut lifecycle = session.lifecycle.write();
        lifecycle.recovery_attempts = lifecycle.recovery_attempts.saturating_add(1);
        lifecycle.phase = CoreLifecyclePhase::Recovering;
        lifecycle.recovery_attempts
    };
    match session
        .maintain_owned(
            &client,
            CoreMaintenanceIntent::Restart,
            policy.ready_timeout,
            &lease,
        )
        .await
    {
        Ok(()) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.phase = CoreLifecyclePhase::Stable;
            lifecycle.last_error = None;
            session.next_generation();
            true
        }
        Err(error) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.last_error = Some(error.to_string());
            lifecycle.phase = if attempt >= policy.max_attempts {
                CoreLifecyclePhase::Failed
            } else {
                CoreLifecyclePhase::Recovering
            };
            false
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
