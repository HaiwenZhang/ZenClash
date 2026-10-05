//! Serialized runtime recovery; capture and administrator maintenance remain caller-owned.

use super::*;

/// Result after a service owner was released and an ordinary Local owner published.
pub struct CoreLocalRecoveryOutcome {
    generation: u64,
    failure: Option<CoreSessionError>,
    binding_generation: u64,
    session_generation: Arc<AtomicU64>,
    bundle: Arc<crate::ServiceRuntimeBundle>,
}

impl CoreLocalRecoveryOutcome {
    pub(crate) fn held_bundle_for(
        &self,
        session: &CoreSession,
    ) -> crate::MihomoResult<Arc<crate::ServiceRuntimeBundle>> {
        if !self.ready()
            || !Arc::ptr_eq(&self.session_generation, &session.generation)
            || self.generation != session.generation()
            || self.binding_generation != session.runtime_descriptor().binding_generation()
            || session.runtime_descriptor().backend() != crate::CoreRuntimeBackend::Local
        {
            return Err(MihomoError::StaleBinding);
        }
        Ok(self.bundle.clone())
    }

    /// Returns the generation published by this recovery completion.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Reports whether ordinary controller readiness was confirmed.
    #[must_use]
    pub const fn ready(&self) -> bool {
        self.failure.is_none()
    }

    /// Returns an unconfirmed child-stop or persistence failure that prevents maintenance.
    #[must_use]
    pub const fn failure(&self) -> Option<&CoreSessionError> {
        self.failure.as_ref()
    }
}

impl CoreSession {
    /// Stops and exports the service runtime, then publishes and starts an ordinary Local owner.
    ///
    /// Recovery uses held resources, disables TUN and preserves the ordinary home.
    /// `fallback` supplies verified ordinary launch identity only when startup had no
    /// preceding Local owner; its source config is never reread as a recovery payload.
    /// The caller coordinates capture intent and releases native capture before admission.
    /// Saves the accepted Local payload and controlled TUN-off setting after readiness.
    /// This does not authorize repair/uninstall or persist a changed system-proxy preference.
    /// Once admitted, completion owns its gates even if its caller stops waiting.
    ///
    /// # Errors
    /// Rejects non-service owners, absent/mismatched ordinary identity, shutdown,
    /// unresolved stop/export/release, validation, publication or confirmed startup failure.
    /// An unconfirmed child stop or persistence failure is returned inside the outcome;
    /// activated resources and any live Local owner remain reachable for shutdown,
    /// and further maintenance must be blocked.
    pub async fn recover_service_to_local(
        &self,
        store: &ControlledConfigStore,
        fallback: Option<crate::MihomoLaunchConfig>,
    ) -> Result<CoreLocalRecoveryOutcome, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let capture = self.capture_publication_gate().lock_owned().await;
        self.recover_service_to_local_admitted(store, fallback, capture, self.generation())
            .await
    }

    pub(crate) async fn recover_service_to_local_admitted(
        &self,
        store: &ControlledConfigStore,
        fallback: Option<crate::MihomoLaunchConfig>,
        capture: tokio::sync::OwnedMutexGuard<()>,
        expected_generation: u64,
    ) -> Result<CoreLocalRecoveryOutcome, CoreSessionError> {
        self.ensure_service_recovery_current(expected_generation)?;
        let client = self.client.pin_binding()?;
        let runtime = client
            .runtime_session()
            .ok_or(CoreSessionError::ReleaseUnsupported { core: self.kind })?;
        let launch = runtime
            .local_launch()
            .cloned()
            .or(fallback)
            .ok_or_else(|| {
                MihomoError::InvalidInput(zenclash_i18n::text("core_page.service.unknown"))
            })?;
        if launch.kind != CoreKind::Mihomo || launch.home_dir != runtime.source_home() {
            return Err(MihomoError::StaleBinding.into());
        }
        let scopes_launch = launch.clone();
        let scopes = tokio::task::spawn_blocking(move || {
            MihomoProcess::prepare_stopped(scopes_launch).write_scopes()
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        self.ensure_service_recovery_current(expected_generation)?;
        let lease = store.acquire_write_lease_for_paths(scopes).await?;
        client.ensure_binding_current()?;
        self.ensure_service_recovery_current(expected_generation)?;
        let store = store.with_write_lease(&lease);
        let transition = self
            .lock_service_recovery_transition(expected_generation)
            .await?;
        client.ensure_binding_current()?;
        self.ensure_not_shutting_down()?;
        let mutation = client.lock_runtime_binding().await?;
        client.ensure_binding_current()?;
        self.ensure_service_recovery_current(expected_generation)?;
        let delta = serde_json::json!({"tun":{"enable":false}});
        self.validate_backup_delta(&delta)?;
        self.lifecycle.write().stop_requested = true;
        let session = self.clone();
        tokio::spawn(async move {
            let _run_attempt = session.begin_core_run_attempt();
            let _capture = capture;
            let _lease = lease;
            let _transition = transition;
            let publisher = session.client.clone();
            let result = runtime
                .recover_local_runtime(
                    &store,
                    launch,
                    session.shutdown_requested.clone(),
                    CORE_READY_TIMEOUT,
                    move |process| async move {
                        publisher.publish_prepared_process(process, mutation).await
                    },
                )
                .await
                .map_err(CoreSessionError::from);
            let mut publication_failure = None;
            let mut ready = result
                .as_ref()
                .is_ok_and(|outcome| outcome.failure.is_none());
            if ready && let Err(error) = session.accept_saved_delta(None, Some(&delta)) {
                ready = false;
                publication_failure = Some(error);
            }
            let generation = session.finish_service_local_recovery(ready);
            result.map(|outcome| CoreLocalRecoveryOutcome {
                generation,
                failure: outcome
                    .failure
                    .map(CoreSessionError::from)
                    .or(publication_failure),
                binding_generation: session.runtime_descriptor().binding_generation(),
                session_generation: session.generation.clone(),
                bundle: outcome.bundle,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    async fn lock_service_recovery_transition(
        &self,
        expected_generation: u64,
    ) -> Result<tokio::sync::OwnedMutexGuard<CommittedConfig>, CoreSessionError> {
        self.ensure_service_recovery_current(expected_generation)?;
        let transition = self.transition.clone().lock_owned().await;
        self.ensure_service_recovery_current(expected_generation)?;
        Ok(transition)
    }

    fn ensure_service_recovery_current(
        &self,
        expected_generation: u64,
    ) -> Result<(), CoreSessionError> {
        self.ensure_not_shutting_down()?;
        if self.generation() != expected_generation {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }

    fn finish_service_local_recovery(&self, ready: bool) -> u64 {
        if ready && !self.is_shutting_down() {
            self.network_suspended.store(false, Ordering::Release);
        }
        let mut lifecycle = self.lifecycle.write();
        lifecycle.stop_requested = !ready || self.is_shutting_down();
        lifecycle.phase = if self.is_shutting_down() {
            CoreLifecyclePhase::ShuttingDown
        } else if ready {
            CoreLifecyclePhase::Stable
        } else {
            CoreLifecyclePhase::Unknown
        };
        drop(lifecycle);
        self.next_generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_local_recovery_blocks_new_maintenance_request() {
        let (session, _) = prepared_receipt().await;
        session.finish_service_local_recovery(false);
        let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-failed-recovery"));
        let capture = crate::TrafficCaptureSession::new(session.clone(), store, None, None);
        let manager = crate::ServiceManager::new(session, capture);
        assert!(matches!(
            manager.request_maintenance(crate::ServiceOperation::Uninstall, None),
            Err(crate::ServiceManagerError::Runtime(_))
        ));
        assert_eq!(manager.snapshot().phase(), crate::ServicePhase::Idle);
    }

    #[tokio::test]
    async fn service_recovery_transition_wait_rejects_changed_configuration() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let expected = session.generation();
        let held = session.transition.clone().lock_owned().await;
        let waiting_session = session.clone();
        let waiting = tokio::spawn(async move {
            waiting_session
                .lock_service_recovery_transition(expected)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        session.mark_runtime_unknown();
        drop(held);
        assert!(matches!(
            waiting.await.unwrap(),
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
    }

    async fn prepared_receipt() -> (CoreSession, CoreLocalRecoveryOutcome) {
        let home =
            std::env::temp_dir().join(format!("zenclash-recovery-receipt-{}", std::process::id()));
        let process = MihomoProcess::prepare_stopped(crate::MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: home.join("ordinary-mihomo"),
            config_file: home.join("held-runtime.yaml"),
            home_dir: home.clone(),
            endpoint: crate::MihomoEndpoint::default(),
            controller_override: None,
        });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process).unwrap(),
        )
        .unwrap();
        let receipt = CoreLocalRecoveryOutcome {
            generation: session.generation(),
            failure: None,
            binding_generation: session.runtime_descriptor().binding_generation(),
            session_generation: session.generation.clone(),
            bundle: Arc::new(
                crate::ServiceRuntimeBundle::prepare("tun:\n  enable: false\n", home)
                    .await
                    .unwrap(),
            ),
        };
        (session, receipt)
    }

    #[tokio::test]
    async fn service_recovery_receipt_allows_same_session_request_and_rejects_changed_generation() {
        let (session, receipt) = prepared_receipt().await;
        let capture = crate::TrafficCaptureSession::new(
            session.clone(),
            ControlledConfigStore::new(std::env::temp_dir().join("recovery-receipt-store")),
            None,
            None,
        );
        let manager = crate::ServiceManager::new(session.clone(), capture);
        assert!(
            manager
                .request_enable_tun_after_recovery(&receipt)
                .unwrap()
                .needs_authorization_consent()
        );
        session.next_generation();
        assert!(matches!(
            manager.request_enable_tun_after_recovery(&receipt),
            Err(crate::ServiceManagerError::Stale)
        ));
    }

    #[tokio::test]
    async fn service_recovery_receipt_rejects_another_session_with_equal_numeric_versions() {
        let (_, receipt) = prepared_receipt().await;
        let (other, _) = prepared_receipt().await;
        assert_eq!(receipt.generation(), other.generation());
        assert_eq!(
            receipt.binding_generation,
            other.runtime_descriptor().binding_generation()
        );
        assert!(receipt.held_bundle_for(&other).is_err());
    }

    #[tokio::test]
    async fn service_recovery_receipt_rejects_unconfirmed_child_stop() {
        let (session, mut receipt) = prepared_receipt().await;
        receipt.failure = Some(MihomoError::StaleBinding.into());
        assert!(receipt.held_bundle_for(&session).is_err());
    }

    #[tokio::test]
    async fn suspended_session_successful_local_recovery_reopens_runtime_operations() {
        let process = MihomoProcess::prepare_stopped(crate::MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: PathBuf::from("ordinary-mihomo"),
            config_file: PathBuf::from("held-runtime.yaml"),
            home_dir: PathBuf::from("ordinary-home"),
            endpoint: crate::MihomoEndpoint::default(),
            controller_override: None,
        });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process).unwrap(),
        )
        .unwrap();
        session
            .admit_network_suspension(async { Ok(()) })
            .await
            .unwrap();
        assert!(session.ensure_running_operations_allowed().is_err());
        session.finish_service_local_recovery(true);
        assert!(session.ensure_running_operations_allowed().is_ok());
        let mut lifecycle = session.lifecycle_snapshot();
        assert!(
            supervisor_observation(
                &mut lifecycle,
                Some((false, None)),
                session.network_suspended.load(Ordering::Acquire),
                false,
                CoreKind::Mihomo,
                3
            )
            .is_some()
        );
    }
}
