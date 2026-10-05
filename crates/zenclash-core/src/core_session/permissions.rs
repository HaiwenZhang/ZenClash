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
                let mihomo = self.kind == CoreKind::Mihomo;
                tokio::task::spawn_blocking(move || {
                    if mihomo {
                        let binary = std::fs::canonicalize(binary).map_err(|error| {
                            crate::TunPermissionError::InvalidBinary(error.to_string())
                        })?;
                        return Ok(crate::TunPermissionStatus {
                            granted: crate::current_process_elevated(),
                            can_request: false,
                            binary,
                            detail: zenclash_i18n::text("startup.local_tun_authority"),
                        });
                    }
                    TunPermissionManager::new(binary)?.status()
                })
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
        if self.kind == CoreKind::Mihomo
            && self.runtime_descriptor().backend() == crate::CoreRuntimeBackend::Local
        {
            return if crate::current_process_elevated() {
                self.ensure_not_shutting_down()
            } else {
                Err(MihomoError::ServiceRequired.into())
            };
        }
        self.ensure_tun_permission_with(|binary| {
            let manager = TunPermissionManager::new(binary)?;
            let already_granted = manager.status()?.granted;
            manager.request_grant()?;
            Ok(already_granted)
        })
        .await
    }

    pub(super) async fn ensure_tun_permission_with(
        &self,
        grant: impl FnOnce(PathBuf) -> Result<bool, crate::TunPermissionError> + Send + 'static,
    ) -> Result<(), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let lease = self.acquire_process_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        let session = self.clone();
        tokio::spawn(async move {
            let _transition = transition;
            let _mutation = mutation;
            match client.owned_core() {
                Some(crate::owned_core::OwnedCore::Local(process)) => {
                    let binary = process.launch_config().binary.clone();
                    let already_granted = tokio::task::spawn_blocking(move || grant(binary))
                        .await
                        .map_err(|error| MihomoError::Process(error.to_string()))?
                        .map_err(|error| MihomoError::Process(error.to_string()))?;
                    client.ensure_binding_current()?;
                    session.ensure_not_shutting_down()?;
                    #[cfg(unix)]
                    if !already_granted {
                        session
                            .restart_after_tun_grant(&process, &lease, CORE_READY_TIMEOUT)
                            .await?;
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

    #[cfg(any(unix, test))]
    pub(super) async fn restart_after_tun_grant(
        &self,
        process: &Arc<MihomoProcess>,
        lease: &DataWriteLease,
        timeout: Duration,
    ) -> Result<(), CoreSessionError> {
        if self.lifecycle.read().stop_requested {
            return Ok(());
        }
        let before_process = process.clone();
        let before = tokio::task::spawn_blocking(move || before_process.snapshot())
            .await
            .map_err(|error| MihomoError::Process(error.to_string()))?;
        let result = process
            .restart_and_wait_until_with_lease(
                timeout,
                Some(self.shutdown_requested.clone()),
                lease,
            )
            .await;
        let after_process = process.clone();
        let after = match tokio::task::spawn_blocking(move || after_process.snapshot()).await {
            Ok(after) => after,
            Err(error) => {
                self.next_generation();
                let mut lifecycle = self.lifecycle.write();
                lifecycle.phase = if self.is_shutting_down() {
                    CoreLifecyclePhase::ShuttingDown
                } else {
                    CoreLifecyclePhase::Unknown
                };
                lifecycle.last_error = Some(error.to_string());
                return Err(MihomoError::Process(error.to_string()).into());
            }
        };
        if result.is_ok() {
            self.next_generation();
        } else if before.pid != after.pid
            || before.running != after.running
            || before.exit_reason != after.exit_reason
        {
            self.next_generation();
            let mut lifecycle = self.lifecycle.write();
            lifecycle.phase = if self.is_shutting_down() {
                CoreLifecyclePhase::ShuttingDown
            } else {
                CoreLifecyclePhase::Unknown
            };
            lifecycle.last_error = result.as_ref().err().map(ToString::to_string);
        }
        result.map_err(CoreSessionError::from)
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}
