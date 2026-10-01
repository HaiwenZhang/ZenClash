use super::*;
use crate::{CoreTunPermissionStatus, Observation, RecoveryAction, TunPermissionManager};

impl CoreSession {
    /// Observes the current runtime's native privilege authority on a background task.
    ///
    /// Service authority requires a fresh authenticated IPC response; unavailable evidence
    /// is never reported as granted. Device and route creation remain separate facts.
    ///
    /// # Errors
    /// Rejects a binding replaced while the observation was in progress.
    pub async fn tun_permission_status(
        &self,
    ) -> crate::MihomoResult<Observation<CoreTunPermissionStatus>> {
        let client = self.client.pin_binding()?;
        let result = match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                let binary = process.launch_config().binary.clone();
                tokio::task::spawn_blocking(move || TunPermissionManager::new(binary)?.status())
                    .await
                    .map_err(|error| MihomoError::Process(error.to_string()))?
                    .map(CoreTunPermissionStatus::Local)
                    .map_err(|error| error.to_string())
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => runtime
                .client
                .status()
                .await
                .map(|_| CoreTunPermissionStatus::Service)
                .map_err(|error| error.to_string()),
            None => {
                return Ok(Observation::Failed {
                    failure: crate::OperationalFailure {
                        message: zenclash_i18n::text(
                            "core_page.errors.runtime_authority_unavailable",
                        ),
                        occurred_at_ms: timestamp(),
                    },
                    recovery: RecoveryAction::Unsupported,
                });
            }
        };
        client.ensure_binding_current()?;
        Ok(Observation::record(
            &Observation::Loading,
            result,
            timestamp(),
            RecoveryAction::ReviewCapture,
        ))
    }

    /// Authorizes the actual local executable, or verifies current administrator service authority.
    ///
    /// # Errors
    /// Rejects shutdown, stale binding, unavailable authority, or failed native authorization.
    pub async fn ensure_tun_permission(&self) -> Result<(), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let session = self.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let lease = session.acquire_process_write_lease(&client).await?;
            let _transition = session.transition.clone().lock_owned().await;
            session.ensure_not_shutting_down()?;
            let _mutation = client.lock_runtime_binding().await?;
            match client.owned_core() {
                Some(crate::owned_core::OwnedCore::Local(process)) => {
                    let binary = process.launch_config().binary.clone();
                    let already_granted = tokio::task::spawn_blocking(move || {
                        let manager = TunPermissionManager::new(binary)?;
                        let already_granted = manager.status()?.granted;
                        manager.request_grant()?;
                        Ok::<_, crate::TunPermissionError>(already_granted)
                    })
                    .await
                    .map_err(|error| MihomoError::Process(error.to_string()))?
                    .map_err(|error| MihomoError::Process(error.to_string()))?;
                    client.ensure_binding_current()?;
                    session.ensure_not_shutting_down()?;
                    #[cfg(unix)]
                    if !already_granted {
                        process
                            .restart_and_wait_until_with_lease(
                                CORE_READY_TIMEOUT,
                                Some(session.shutdown_requested.clone()),
                                &lease,
                            )
                            .await?;
                        session.next_generation();
                    }
                    #[cfg(not(unix))]
                    let _ = (already_granted, &lease);
                }
                Some(crate::owned_core::OwnedCore::Service(runtime)) => {
                    runtime.client.status().await.map_err(MihomoError::from)?;
                }
                None => {
                    return Err(CoreSessionError::ExternalRestartUnsupported {
                        core: session.kind,
                    });
                }
            }
            client.ensure_binding_current()?;
            session.ensure_not_shutting_down()?;
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}
