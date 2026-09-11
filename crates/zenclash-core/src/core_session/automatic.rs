use super::*;

impl CoreSession {
    /// Immediately prevents new automatic work while asynchronous exit cleanup runs.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Stops an owned running core after link loss without erasing its capture intent.
    ///
    /// # Errors
    /// Reports native capture release, process shutdown, or application exit failures.
    pub async fn suspend_for_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        {
            let _transition = self.transition.lock().await;
            self.ensure_not_shutting_down()?;
            let Some(process) = &self.process else {
                return Ok(false);
            };
            if !process.is_running() || self.lifecycle.read().phase == CoreLifecyclePhase::Stopped {
                return Ok(false);
            }
            self.network_suspended.store(true, Ordering::Release);
            self.lifecycle.write().phase = CoreLifecyclePhase::NetworkSuspended;
        }
        // Capture operations may themselves wait for the core transition gate.
        let released = CoreRecoveryCapture::release_owned(capture).await;
        let _transition = self.transition.lock().await;
        self.ensure_not_shutting_down()?;
        if !self.network_suspended.load(Ordering::Acquire) {
            return Ok(false);
        }
        if let Some(process) = &self.process {
            process.stop_async().await?;
        }
        self.next_generation();
        released.map_err(|error| CoreSessionError::Process(MihomoError::Process(error)))?;
        Ok(true)
    }

    /// Resumes only a core previously suspended by link loss and reconciles capture.
    ///
    /// # Errors
    /// Returns readiness, capture recovery, or application exit failures. A failed
    /// attempt remains eligible for a later network-recovery retry.
    pub async fn resume_after_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        {
            let _transition = self.transition.lock().await;
            self.ensure_not_shutting_down()?;
            if !self.network_suspended.load(Ordering::Acquire) {
                return Ok(false);
            }
            let Some(process) = &self.process else {
                return Ok(false);
            };
            if !process.is_running() {
                process
                    .restart_and_wait_until(
                        CORE_READY_TIMEOUT,
                        Some(self.shutdown_requested.clone()),
                    )
                    .await?;
                self.next_generation();
            }
        }
        CoreRecoveryCapture::reconcile(capture)
            .await
            .map_err(|error| CoreSessionError::Process(MihomoError::Process(error)))?;
        let _transition = self.transition.lock().await;
        self.ensure_not_shutting_down()?;
        if self.network_suspended.swap(false, Ordering::AcqRel) {
            *self.lifecycle.write() = CoreLifecycleSnapshot::new(true);
            return Ok(true);
        }
        Ok(false)
    }

    /// Restarts a changed active source only if the observed core generation is current.
    ///
    /// # Errors
    /// Returns validation, source, restart or rollback failures. External and
    /// intentionally stopped cores are left untouched.
    pub async fn restart_changed_source(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        expected_generation: u64,
        expected_revision: [u8; 32],
    ) -> Result<bool, CoreSessionError> {
        let _transition = self.transition.lock().await;
        self.ensure_not_shutting_down()?;
        let Some(process) = &self.process else {
            return Ok(false);
        };
        if !process.is_running()
            || self.network_suspended.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != expected_generation
        {
            return Ok(false);
        }
        let check_store = store.clone();
        let check_profile = profile.clone();
        let check_overrides = overrides.clone();
        let kind = self.kind;
        let current = tokio::task::spawn_blocking(move || {
            if check_store.pending_source_revision(kind, &check_profile, &check_overrides)?
                != Some(expected_revision)
            {
                return Ok::<_, ControlledConfigError>(false);
            }
            Ok(true)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        if !current {
            return Ok(false);
        }
        self.ensure_not_shutting_down()?;
        store
            .stage_profile_restart(process.clone(), profile, overrides)
            .await?
            .commit();
        self.next_generation();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn automatic_network_actions_never_control_an_external_process() {
        let client = MihomoClient::new(crate::MihomoEndpoint::default()).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client, None);
        let capture = TrafficCaptureSession::new(
            session.clone(),
            ControlledConfigStore::new(std::env::temp_dir()),
            None,
            None,
            None,
        );
        assert!(!session.suspend_for_network(&capture).await.unwrap());
        assert!(!session.resume_after_network(&capture).await.unwrap());
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::External
        );
    }
}
