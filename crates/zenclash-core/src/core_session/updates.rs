//! Managed executable activation and recovery.

use super::*;

impl CoreSession {
    /// Downloads a Mihomo release, then serializes executable activation and recovery.
    ///
    /// Dropping the caller does not abandon an activated file transaction. Application
    /// shutdown cancels preparation and prevents both candidate and recovery restarts.
    ///
    /// # Errors
    /// Returns unsupported-core, lifecycle, release verification or recovery errors.
    pub async fn install_release(
        &self,
        service: &MihomoReleaseService,
        release: &MihomoRelease,
    ) -> Result<CoreInstallOutcome, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        if self.kind != CoreKind::Mihomo {
            return Err(CoreSessionError::ReleaseUnsupported { core: self.kind });
        }
        let session = self.clone();
        let service = service.clone();
        let release = release.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let process = session.local_process(&client)?;
            let lease = session.acquire_runtime_write_lease(&client).await?;
            let client = client.with_write_lease(&lease)?;
            session.ensure_not_shutting_down()?;
            let prepared = service
                .prepare_until(
                    &release,
                    process.launch_config().binary.clone(),
                    Some(session.shutdown_requested.clone()),
                )
                .await?;
            let _transition = session.transition.lock().await;
            if let Err(error) = session.ensure_not_shutting_down().and_then(|()| {
                client
                    .ensure_binding_current()
                    .map_err(CoreSessionError::from)
            }) {
                tokio::task::spawn_blocking(move || drop(prepared))
                    .await
                    .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
                return Err(error);
            }
            let _mutation = client.lock_runtime_binding().await?;
            let _availability_pause = session.pause_pac_availability();
            let previous_pid = process.snapshot().pid;
            let previous_phase = session.lifecycle.read().phase;
            let result = service
                .install_prepared(
                    prepared,
                    process.clone(),
                    client,
                    session.shutdown_requested.clone(),
                    &lease,
                )
                .await;
            session.client.invalidate_connections();
            match result {
                Ok(crate::core_update::InstalledCore {
                    version,
                    cleanup_error,
                }) => {
                    let generation = session.next_generation();
                    session.network_suspended.store(false, Ordering::Release);
                    let mut lifecycle = session.lifecycle.write();
                    *lifecycle = CoreLifecycleSnapshot::new(true);
                    lifecycle.last_error = cleanup_error.clone();
                    if session.is_shutting_down() {
                        lifecycle.stop_requested = true;
                        lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
                    }
                    Ok(CoreInstallOutcome {
                        version,
                        generation,
                        cleanup_error,
                    })
                }
                Err(error) => {
                    let snapshot = process.snapshot();
                    if snapshot.pid != previous_pid {
                        session.next_generation();
                    }
                    let running = snapshot.running;
                    let mut lifecycle = session.lifecycle.write();
                    lifecycle.phase = if session.is_shutting_down() {
                        CoreLifecyclePhase::ShuttingDown
                    } else if running {
                        CoreLifecyclePhase::Stable
                    } else if matches!(
                        previous_phase,
                        CoreLifecyclePhase::Stopped | CoreLifecyclePhase::NetworkSuspended
                    ) {
                        previous_phase
                    } else {
                        CoreLifecyclePhase::Failed
                    };
                    lifecycle.last_error = Some(error.to_string());
                    Err(error.into())
                }
            }
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}
