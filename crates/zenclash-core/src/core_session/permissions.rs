use super::*;
use crate::{CoreTunPermissionStatus, Observation, RecoveryAction};

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
                tokio::task::spawn_blocking(
                    move || -> crate::TunPermissionResult<crate::TunPermissionStatus> {
                        let binary = std::fs::canonicalize(binary).map_err(|error| {
                            crate::TunPermissionError::InvalidBinary(error.to_string())
                        })?;
                        Ok(crate::TunPermissionStatus {
                            granted: crate::current_process_elevated(),
                            can_request: false,
                            binary,
                            detail: zenclash_i18n::text("startup.local_tun_authority"),
                        })
                    },
                )
                .await
                .map_err(|error| MihomoError::Process(error.to_string()))?
                .map(CoreTunPermissionStatus::Local)
                .map_err(|error| error.to_string())
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => {
                match runtime.client.status().await {
                    Ok(status) if runtime.client.owns_status(&status) => {
                        Ok(CoreTunPermissionStatus::Service)
                    }
                    Ok(_) => Err(zenclash_i18n::text(
                        "core_page.errors.runtime_authority_unavailable",
                    )),
                    Err(error) => Err(error.to_string()),
                }
            }
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

    /// Verifies TUN authority for one pinned runtime without granting privileges or restarting it.
    /// Local cores require existing process authority; Service cores require an authenticated read.
    ///
    /// # Errors
    /// Rejects shutdown, stale binding, unprivileged Local cores, unavailable Service authority,
    /// or external runtimes whose privileges the application does not own.
    pub async fn ensure_tun_permission(&self) -> Result<(), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let _lease = self.acquire_runtime_write_lease(&client).await?;
        let _transition = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let _mutation = client.lock_runtime_binding().await?;
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(_)) => {
                if !crate::current_process_elevated() {
                    return Err(MihomoError::ServiceRequired.into());
                }
            }
            Some(crate::owned_core::OwnedCore::Service(runtime)) => {
                let status = runtime.client.status().await.map_err(MihomoError::from)?;
                if !runtime.client.owns_status(&status) {
                    return Err(MihomoError::StaleBinding.into());
                }
            }
            None => return Err(CoreSessionError::ExternalRestartUnsupported { core: self.kind }),
        }
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}
