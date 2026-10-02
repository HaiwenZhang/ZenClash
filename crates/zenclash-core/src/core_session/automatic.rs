use super::*;

impl CoreSession {
    /// Immediately closes runtime publication admission while asynchronous cleanup runs.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.client.close_runtime_admission();
        let mut lifecycle = self.lifecycle.write();
        lifecycle.stop_requested = true;
        lifecycle.phase = CoreLifecyclePhase::ShuttingDown;
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Stops the actual owned core after link loss while retaining its accepted runtime.
    ///
    /// # Errors
    /// Returns capture release, stale admission, native Stop confirmation or shutdown errors.
    pub async fn suspend_for_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        let session = self.clone();
        let capture = capture.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            {
                let _lease = session.acquire_process_write_lease(&client).await?;
                let _transition = session.transition.lock().await;
                client.ensure_binding_current()?;
                session.ensure_not_shutting_down()?;
                if client.owned_core().is_none() || session.lifecycle.read().stop_requested {
                    return Ok(false);
                }
                let _mutation = client.lock_runtime_binding().await?;
                session
                    .admit_network_suspension(async {
                        if let Some(runtime) = client.runtime_session() {
                            runtime.confirm_finalizing_before_stop().await?;
                        }
                        Ok(())
                    })
                    .await?;
            }
            // Capture may itself acquire Transition. Recheck the same pin afterward.
            let released = CoreRecoveryCapture::release_owned(&capture).await;
            let lease = session.acquire_process_write_lease(&client).await?;
            let _transition = session.transition.clone().lock_owned().await;
            client.ensure_binding_current()?;
            session.ensure_not_shutting_down()?;
            if !session.network_suspended.load(Ordering::Acquire) {
                return Ok(false);
            }
            let _mutation = client.lock_runtime_binding().await?;
            if let Err(error) = session
                .maintain_owned(
                    &client,
                    CoreMaintenanceIntent::Stop,
                    CORE_READY_TIMEOUT,
                    &lease,
                )
                .await
            {
                session.lifecycle.write().phase = CoreLifecyclePhase::Unknown;
                return Err(error);
            }
            session.next_generation();
            released.map_err(|error| CoreSessionError::Process(MihomoError::Process(error)))?;
            Ok(true)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(super) async fn admit_network_suspension(
        &self,
        confirmation: impl std::future::Future<Output = crate::MihomoResult<()>>,
    ) -> Result<(), CoreSessionError> {
        confirmation.await?;
        self.ensure_not_shutting_down()?;
        self.network_suspended.store(true, Ordering::Release);
        self.lifecycle.write().phase = CoreLifecyclePhase::NetworkSuspended;
        Ok(())
    }

    /// Resumes the accepted runtime after link loss and restores capture intent.
    ///
    /// # Errors
    /// Returns restart confirmation, capture, stale admission or shutdown failures.
    pub async fn resume_after_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        let session = self.clone();
        let capture = capture.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            {
                let lease = session.acquire_process_write_lease(&client).await?;
                let _transition = session.transition.clone().lock_owned().await;
                client.ensure_binding_current()?;
                session.ensure_not_shutting_down()?;
                if !session.network_suspended.load(Ordering::Acquire)
                    || client.owned_core().is_none()
                    || session.lifecycle.read().stop_requested
                {
                    return Ok(false);
                }
                let _mutation = client.lock_runtime_binding().await?;
                if let Err(error) = session
                    .maintain_owned(
                        &client,
                        CoreMaintenanceIntent::Restart,
                        CORE_READY_TIMEOUT,
                        &lease,
                    )
                    .await
                {
                    session.lifecycle.write().phase = CoreLifecyclePhase::Unknown;
                    return Err(error);
                }
                session.next_generation();
            }
            CoreRecoveryCapture::reconcile(&capture)
                .await
                .map_err(|error| CoreSessionError::Process(MihomoError::Process(error)))?;
            let _transition = session.transition.lock().await;
            client.ensure_binding_current()?;
            session.ensure_not_shutting_down()?;
            if session.network_suspended.swap(false, Ordering::AcqRel) {
                *session.lifecycle.write() = CoreLifecycleSnapshot::new(true);
                Ok(true)
            } else {
                Ok(false)
            }
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    /// Applies a changed source only while its observed runtime generation remains current.
    ///
    /// # Errors
    /// Returns source validation, configuration transaction, stale admission or shutdown errors.
    pub async fn restart_changed_source(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        expected_generation: u64,
        expected_revision: [u8; 32],
    ) -> Result<bool, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mut active_profile = self.transition.lock().await;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        if client.owned_core().is_none()
            || self.snapshot().running != Some(true)
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
            Ok::<_, ControlledConfigError>(
                check_store.pending_source_revision(kind, &check_profile, &check_overrides)?
                    == Some(expected_revision),
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        if !current {
            return Ok(false);
        }
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                let _mutation = client.lock_runtime_binding().await?;
                store
                    .stage_profile_restart(
                        process,
                        profile.clone(),
                        overrides.clone(),
                        Some(self.shutdown_requested.clone()),
                    )
                    .await
                    .map_err(|error| self.runtime_mutation_error(error))?
                    .commit()
                    .await?;
            }
            Some(crate::owned_core::OwnedCore::Service(_)) => {
                store
                    .stage_profile_reload(
                        &client,
                        profile.clone(),
                        active_profile.profile.clone(),
                        overrides.clone(),
                    )
                    .await
                    .map_err(|error| self.runtime_mutation_error(error))?
                    .commit()
                    .await?;
            }
            None => return Ok(false),
        }
        let committed = CommittedConfig {
            profile: Some(profile),
            overrides,
        };
        *active_profile = committed.clone();
        self.next_generation_with_config(Some(committed));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn automatic_network_actions_never_control_an_external_process() {
        let client = MihomoClient::new(crate::MihomoEndpoint::default()).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let capture = TrafficCaptureSession::new(
            session.clone(),
            ControlledConfigStore::new(std::env::temp_dir()),
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
