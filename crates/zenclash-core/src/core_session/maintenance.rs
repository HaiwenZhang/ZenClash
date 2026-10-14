//! Explicit restart, stop, service export and shutdown of the existing owner.

use super::*;

impl CoreSession {
    /// Performs a serialized managed-core maintenance transition.
    /// Cancellation while queued has no effects. Once admitted, completion retains
    /// the transaction guards even if the caller stops waiting.
    ///
    /// # Errors
    ///
    /// Returns an error for external cores or when restart/readiness fails.
    pub async fn maintain(&self, intent: CoreMaintenanceIntent) -> Result<u64, CoreSessionError> {
        self.maintain_with_timeout(intent, CORE_READY_TIMEOUT).await
    }

    /// Confirms service-kernel shutdown and exports its accepted resources and caches.
    /// Returns the published generation and an immutable ordinary-user recovery bundle.
    /// Once admitted, completion retains the transition even if its caller is cancelled.
    /// This does not release the service, perform administrator maintenance, or persist files.
    ///
    /// # Errors
    /// Rejects non-service owners, shutdown, unresolved revisions, and export failures.
    /// Failure retains the prior accepted bundle and the explicit stop intention.
    pub async fn export_service_runtime(
        &self,
    ) -> Result<(u64, Arc<crate::ServiceRuntimeBundle>), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let lease = self.acquire_runtime_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        let Some(crate::owned_core::OwnedCore::Service(runtime)) = client.owned_core() else {
            return Err(CoreSessionError::ReleaseUnsupported { core: self.kind });
        };
        self.ensure_not_shutting_down()?;
        self.lifecycle.write().stop_requested = true;
        self.network_suspended.store(false, Ordering::Release);
        let session = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let _availability_pause = session.pause_pac_availability();
            let _transition = transition;
            let _mutation = mutation;
            let result = runtime
                .stop_and_export()
                .await
                .map_err(CoreSessionError::Process);
            {
                let mut lifecycle = session.lifecycle.write();
                record_maintenance_outcome(&mut lifecycle, CoreMaintenanceIntent::Stop, &result);
                if session.is_shutting_down() {
                    lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                }
            }
            let generation = session.next_generation();
            Ok((generation, result?))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(super) async fn maintain_with_timeout(
        &self,
        intent: CoreMaintenanceIntent,
        timeout: Duration,
    ) -> Result<u64, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let lease = self.acquire_runtime_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        let owner = client
            .owned_core()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })?;
        let before = match &owner {
            crate::owned_core::OwnedCore::Local(process) => {
                let process = process.clone();
                tokio::task::spawn_blocking(move || process.try_snapshot())
                    .await
                    .map_err(|error| MihomoError::Process(error.to_string()))?
                    .ok()
            }
            crate::owned_core::OwnedCore::Service(_) => None,
        };
        self.ensure_not_shutting_down()?;
        let previous_network_suspended = self.network_suspended.swap(false, Ordering::AcqRel);
        let previous_lifecycle = {
            let mut lifecycle = self.lifecycle.write();
            let previous = (lifecycle.phase, lifecycle.stop_requested);
            lifecycle.stop_requested = intent == CoreMaintenanceIntent::Stop;
            previous
        };
        let local_observed = before.is_some();
        let session = self.clone();
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _transition = transition;
            let _mutation = mutation;
            let result = session
                .maintain_owned(&client, intent, timeout, &lease)
                .await;
            let changed = match owner {
                crate::owned_core::OwnedCore::Local(process) => {
                    let after = tokio::task::spawn_blocking(move || process.try_snapshot()).await;
                    match (before, after) {
                        (Some(before), Ok(Ok(after))) => before.pid != after.pid
                            || before.running != after.running || before.exit_reason != after.exit_reason,
                        _ => true,
                    }
                }
                crate::owned_core::OwnedCore::Service(_) => result.as_ref().err().is_some_and(|error| {
                    matches!(error, CoreSessionError::Process(error) if error.mutation_result_unknown())
                }),
            };
            {
                let mut lifecycle = session.lifecycle.write();
                if intent == CoreMaintenanceIntent::Restart
                    && result.is_err()
                    && local_observed
                    && !changed
                {
                    // Validation and TUN rejection cannot revoke an unchanged local owner.
                    lifecycle.phase = previous_lifecycle.0;
                    lifecycle.stop_requested = previous_lifecycle.1;
                    session
                        .network_suspended
                        .store(previous_network_suspended, Ordering::Release);
                } else {
                    record_maintenance_outcome(&mut lifecycle, intent, &result);
                }
                if session.is_shutting_down() {
                    lifecycle.stop_requested = true;
                    lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                }
            }
            if result.is_err() && changed {
                session.next_generation();
            }
            result?;
            Ok(session.next_generation())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(super) async fn maintain_owned(
        &self,
        client: &MihomoClient,
        intent: CoreMaintenanceIntent,
        timeout: Duration,
        lease: &DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => match intent {
                CoreMaintenanceIntent::Restart => {
                    process
                        .restart_and_wait_until_with_lease(
                            timeout,
                            Some(self.shutdown_requested.clone()),
                            lease,
                        )
                        .await?
                }
                CoreMaintenanceIntent::Stop => process.stop_async().await?,
            },
            Some(crate::owned_core::OwnedCore::Service(runtime)) => match intent {
                CoreMaintenanceIntent::Restart => {
                    runtime.restart_accepted(&self.shutdown_requested).await?
                }
                CoreMaintenanceIntent::Stop => runtime.stop_confirmed().await?,
            },
            None => return Err(CoreSessionError::ExternalRestartUnsupported { core: self.kind }),
        }
        Ok(())
    }

    /// Stops the managed child, or detaches from an external controller.
    ///
    /// # Errors
    ///
    /// Returns an error when an owned child cannot be stopped and reaped.
    pub async fn shutdown(&self) -> Result<(), CoreSessionError> {
        self.request_shutdown();
        let session = self.clone();
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _data_application = session.data_application_gate.clone().lock_owned().await;
            let _capture = session.capture_publication_gate().lock_owned().await;
            let _transition = session.transition.clone().lock_owned().await;
            let client = session.client.pin_binding()?;
            let _mutation = client.lock_runtime_binding().await?;
            session.lifecycle.write().phase = CoreLifecyclePhase::ShuttingDown;
            let result = match client.owned_core() {
                Some(crate::owned_core::OwnedCore::Local(process)) => {
                    process.stop_async().await.map_err(CoreSessionError::from)
                }
                Some(crate::owned_core::OwnedCore::Service(runtime)) => runtime
                    .release_owned()
                    .await
                    .map_err(CoreSessionError::from),
                None => Ok(()),
            };
            session.next_generation();
            session.lifecycle.write().phase = if result.is_ok() {
                CoreLifecyclePhase::Stopped
            } else {
                CoreLifecyclePhase::Unknown
            };
            result
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

pub(crate) fn record_maintenance_outcome<T>(
    lifecycle: &mut CoreLifecycleSnapshot,
    intent: CoreMaintenanceIntent,
    result: &Result<T, CoreSessionError>,
) {
    lifecycle.stop_requested = intent == CoreMaintenanceIntent::Stop;
    lifecycle.phase = if result.is_err() {
        CoreLifecyclePhase::Unknown
    } else {
        match intent {
            CoreMaintenanceIntent::Restart => CoreLifecyclePhase::Stable,
            CoreMaintenanceIntent::Stop => CoreLifecyclePhase::Stopped,
        }
    };
}

#[cfg(all(test, unix))]
mod tests;
