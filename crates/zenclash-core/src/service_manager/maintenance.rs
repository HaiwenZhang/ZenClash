//! Runtime preparation for explicit repair and uninstall commands.

use super::*;

/// One repair or uninstall intent bound to the application's actual CoreSession.
///
/// Creating it does not read files, stop capture or grant native authorization.
#[derive(Clone)]
pub struct ServiceMaintenanceRequest {
    operation: ServiceOperation,
    intent: ServiceIntent,
    capture_gate: Arc<Mutex<()>>,
    backend: CoreRuntimeBackend,
    ordinary_launch: Option<crate::MihomoLaunchConfig>,
}

impl ServiceMaintenanceRequest {
    /// Reads the explicit repair or uninstall operation.
    #[must_use]
    pub const fn operation(&self) -> ServiceOperation {
        self.operation
    }
}

/// Prepared runtime state before administrator service maintenance.
///
/// Retains scoped resources for failure/cancellation recovery. It is not a
/// successful maintenance receipt and grants no administrator permission.
#[derive(Clone)]
pub struct ServiceMaintenancePreparation {
    request: ServiceMaintenanceRequest,
    recovery: Option<Arc<crate::ServiceLocalRecoveryOutcome>>,
    allow_authorization: bool,
}

impl ServiceMaintenancePreparation {
    /// Allows the native prompt after explicit confirmation of this prepared intent.
    #[must_use]
    pub fn with_authorization(mut self) -> Self {
        self.allow_authorization = true;
        self
    }

    /// Reads the operation awaiting native authorization.
    #[must_use]
    pub const fn operation(&self) -> ServiceOperation {
        self.request.operation
    }

    /// Reads capture observations and Local recovery facts, when Service was left.
    #[must_use]
    pub fn recovery(&self) -> Option<&crate::ServiceLocalRecoveryOutcome> {
        self.recovery.as_deref()
    }

    /// Reports whether Service recovery permits maintenance; existing Local needs no recovery.
    /// Callers must also recheck current intent before native submission.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.recovery
            .as_ref()
            .is_none_or(|recovery| recovery.runtime().ready())
    }
}

/// Capture restoration facts retained separately from the native maintenance result.
pub struct ServiceMaintenanceRecoveryOutcome {
    tun: Option<crate::ServiceTunOutcome>,
    warning: Option<String>,
}

impl ServiceMaintenanceRecoveryOutcome {
    /// Reads a durable TUN receipt even when a later proxy restoration failed.
    #[must_use]
    pub const fn tun(&self) -> Option<&crate::ServiceTunOutcome> {
        self.tun.as_ref()
    }

    /// Reads a capture restoration or readback failure requiring reconciliation.
    #[must_use]
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

impl ServiceManager {
    /// Restores capture after a confirmed repair result using held recovery resources.
    /// Never prompts for authorization. Callers must not invoke this after an
    /// unconfirmed native result; command admission also rejects that state.
    /// Owned proxy restoration preserves an advanced proxy/TUN combination.
    ///
    /// # Errors
    /// Rejects stale/unready recovery, shutdown, non-repair operations, concurrent
    /// commands, unhealthy installation or failed TUN handover. A later proxy
    /// failure is carried as a warning without discarding a durable TUN receipt.
    pub async fn restore_prepared_capture(
        &self,
        preparation: &ServiceMaintenancePreparation,
    ) -> Result<ServiceMaintenanceRecoveryOutcome, ServiceManagerError> {
        self.check_maintenance_preparation(preparation)?;
        if preparation.operation() != ServiceOperation::Repair {
            return Err(ServiceManagerError::Unsupported);
        }
        let preparation = preparation.clone();
        self.complete(ServiceOperation::Repair, move |manager| async move {
            manager.check_maintenance_preparation(&preparation)?;
            let Some(recovery) = preparation.recovery() else {
                return Ok(ServiceMaintenanceRecoveryOutcome {
                    tun: None,
                    warning: None,
                });
            };
            if manager.capture.capture_revision() != recovery.capture_revision() {
                return Err(ServiceManagerError::Stale);
            }
            if !recovery.before().tun.is_fresh() {
                return Err(ServiceManagerError::Runtime(Box::new(
                    crate::MihomoError::Process(zenclash_i18n::text(
                        "core_page.service.capture_restore_unknown",
                    ))
                    .into(),
                )));
            }
            let mut request = manager.request_enable_tun_after_recovery(recovery.runtime())?;
            request.expected_capture_revision = Some(recovery.capture_revision());
            let health = manager.observe_health().await;
            manager
                .state
                .send_modify(|state| state.health = Some(Arc::new(health.clone())));
            manager.check_maintenance_preparation(&preparation)?;
            if health != ServiceHealth::Ready {
                return Err(ServiceManagerError::Health(health));
            }
            manager
                .state
                .send_modify(|state| state.phase = ServicePhase::Restoring);
            let before = recovery.before();
            let tun =
                if before.tun.is_fresh() && before.tun.value().is_some_and(|tun| tun.configured) {
                    Some(manager.enable_tun_admitted(request).await?)
                } else {
                    None
                };
            // A saved handover with unresolved commit/readback cannot authorize
            // a further native proxy write, but its receipt must reach business state.
            let tun_warning = tun.as_ref().and_then(|outcome| {
                if outcome.commit_pending() {
                    Some(zenclash_i18n::text("core_page.service.pending"))
                } else {
                    outcome.recovery_warning().map(str::to_owned).or_else(|| {
                        match outcome.capture() {
                            crate::CaptureOutcome::RolledBack { failure, .. }
                            | crate::CaptureOutcome::ReconcileNeeded { failure, .. } => {
                                Some(failure.clone())
                            }
                            _ => None,
                        }
                    })
                }
            });
            let warning = if tun_warning.is_some() {
                tun_warning
            } else {
                let generation = tun
                    .as_ref()
                    .and_then(|outcome| outcome.core())
                    .map_or(preparation.request.intent.generation, |core| {
                        core.generation
                    });
                let binding = if tun.is_some() {
                    manager.session.runtime_descriptor().binding_generation()
                } else {
                    preparation.request.intent.binding
                };
                manager
                    .capture
                    .restore_maintenance_proxy(
                        &manager.session,
                        before.clone(),
                        binding,
                        generation,
                        recovery.capture_revision(),
                    )
                    .await
                    .err()
                    .map(|error| error.to_string())
            };
            Ok(ServiceMaintenanceRecoveryOutcome { tun, warning })
        })
        .await
    }

    /// Resolves missing ordinary identity for a directly started Service before confirmation.
    /// Existing Local identity is returned without filesystem work. Discovery follows
    /// normal executable preferences, retains accepted source/home paths and never
    /// reads source YAML, stops capture or requests administrator authorization.
    ///
    /// # Errors
    /// Rejects unsupported operations/owners, missing accepted identity, stale intent,
    /// overlapping commands and failed ordinary executable discovery.
    pub async fn discover_maintenance_request(
        &self,
        operation: ServiceOperation,
        project_root: PathBuf,
        preferred_binary: Option<PathBuf>,
    ) -> Result<ServiceMaintenanceRequest, ServiceManagerError> {
        if !matches!(
            operation,
            ServiceOperation::Repair | ServiceOperation::Uninstall
        ) {
            return Err(ServiceManagerError::Unsupported);
        }
        if self.session.is_offline_recovery() {
            return self.request_maintenance(operation, None);
        }
        let tun = self.request_enable_tun()?;
        self.ensure_recovery_confirmed()?;
        if self.session.local_recovery_launch().is_some() {
            return self.request_maintenance(operation, None);
        }
        if tun.backend != CoreRuntimeBackend::Service {
            return Err(ServiceManagerError::Unsupported);
        }
        let home = self
            .session
            .client()
            .runtime_session()
            .ok_or(ServiceManagerError::Unsupported)?
            .source_home()
            .to_path_buf();
        let profile = self
            .session
            .committed_profile_snapshot()
            .profile_path
            .ok_or(ServiceManagerError::Unsupported)?;
        self.complete(operation, move |manager| async move {
            manager.check_intent(tun.intent)?;
            let fallback = crate::MihomoLaunchConfig::discover_ordinary_recovery(
                project_root,
                preferred_binary,
                profile,
                home,
            )
            .await
            .map_err(|error| ServiceManagerError::Runtime(Box::new(error.into())))?;
            manager.check_intent(tun.intent)?;
            manager.request_maintenance(operation, Some(fallback))
        })
        .await
    }

    /// Submits native repair or removal after runtime preparation and explicit consent.
    ///
    /// The caller retains `preparation` for recovery after cancellation/failure.
    /// This only maintains the installation; it does not restore capture, save
    /// capture preferences or automatically return the Local runtime to Service.
    /// No capture/transition lock is held during the native authorization prompt.
    ///
    /// # Errors
    /// Rejects stale/foreign preparation, unconfirmed Local readiness, shutdown,
    /// concurrent commands, untrusted health/sources and native maintenance failure.
    pub async fn maintain_prepared(
        &self,
        preparation: &ServiceMaintenancePreparation,
    ) -> Result<(), ServiceManagerError> {
        self.check_maintenance_preparation(preparation)?;
        if !preparation.allow_authorization {
            return Err(ServiceManagerError::ConsentRequired);
        }
        let preparation = preparation.clone();
        self.complete(preparation.operation(), move |manager| async move {
            manager.check_maintenance_preparation(&preparation)?;
            let request = &preparation.request;
            let health = manager.observe_health().await;
            manager
                .state
                .send_modify(|state| state.health = Some(Arc::new(health.clone())));
            manager.check_maintenance_preparation(&preparation)?;
            let Some(action) = maintenance_action(request.operation, &health)? else {
                // Already absent: removal is complete without an unnecessary OS prompt.
                return Ok(());
            };
            let core = if request.operation == ServiceOperation::Repair {
                Some(manager.repair_core_source(request).await?)
            } else {
                None
            };
            manager.check_maintenance_preparation(&preparation)?;
            let helper = super::tun::bundled_helper().await?;
            manager
                .authorize_action(
                    request.intent,
                    action,
                    zenclash_service_integration::maintenance::maintain_service(
                        action, helper, core,
                    ),
                )
                .await
        })
        .await
    }

    async fn repair_core_source(
        &self,
        request: &ServiceMaintenanceRequest,
    ) -> Result<PathBuf, ServiceManagerError> {
        self.check_maintenance_request(request)?;
        let ordinary = request
            .ordinary_launch
            .as_ref()
            .map(|launch| launch.binary.clone());
        let offline = self
            .session
            .offline_source()
            .map(|source| (source.home.clone(), source.binary.clone()));
        let binary = tokio::task::spawn_blocking(move || {
            let binary = match (ordinary, offline) {
                (Some(binary), _) | (None, Some((_, Some(binary)))) => binary,
                (None, Some((home, None))) => crate::process::service_core_source(&home)?,
                (None, None) => {
                    return Err(crate::MihomoError::InvalidInput(
                        "No application-owned service core source".into(),
                    ));
                }
            };
            verify_ordinary_local_executable(&binary)?;
            Ok::<_, crate::MihomoError>(binary)
        })
        .await
        .map_err(ServiceManagerError::Completion)?
        .map_err(|error| ServiceManagerError::Runtime(Box::new(error.into())))?;
        self.check_maintenance_request(request)?;
        Ok(binary)
    }

    fn check_maintenance_preparation(
        &self,
        preparation: &ServiceMaintenancePreparation,
    ) -> Result<(), ServiceManagerError> {
        self.check_maintenance_request(&preparation.request)?;
        self.ensure_recovery_confirmed()?;
        if preparation.request.backend != CoreRuntimeBackend::Local
            && !(preparation.request.backend == CoreRuntimeBackend::Direct
                && self.session.is_offline_recovery())
        {
            return Err(ServiceManagerError::Unsupported);
        }
        if !preparation.ready() {
            return Err(ServiceManagerError::Runtime(Box::new(
                crate::MihomoError::Process(zenclash_i18n::text(
                    "core_page.service.cleanup_unconfirmed",
                ))
                .into(),
            )));
        }
        Ok(())
    }

    /// Captures a repair/uninstall request for the runtime currently shown.
    ///
    /// A directly started Service may supply ordinary startup identity as a
    /// fallback. Existing Local identity takes precedence; no protected helper
    /// executable is inferred as a Local recovery source.
    ///
    /// # Errors
    /// Rejects other operations, mismatched capture owners, shutdown, external
    /// or experimental runtimes and missing ordinary Service recovery identity.
    pub fn request_maintenance(
        &self,
        operation: ServiceOperation,
        fallback: Option<crate::MihomoLaunchConfig>,
    ) -> Result<ServiceMaintenanceRequest, ServiceManagerError> {
        if !matches!(
            operation,
            ServiceOperation::Repair | ServiceOperation::Uninstall
        ) {
            return Err(ServiceManagerError::Unsupported);
        }
        if self.session.runtime_descriptor().kind() == CoreKind::Mihomo
            && self.session.is_offline_recovery()
        {
            let request = ServiceMaintenanceRequest {
                operation,
                intent: self.intent(),
                capture_gate: self.session.capture_publication_gate(),
                backend: CoreRuntimeBackend::Direct,
                ordinary_launch: None,
            };
            self.check_maintenance_request(&request)?;
            self.ensure_recovery_confirmed()?;
            return Ok(request);
        }
        let tun = self.request_enable_tun()?;
        self.ensure_recovery_confirmed()?;
        let ordinary_launch = self.session.local_recovery_launch().or(fallback);
        if ordinary_launch
            .as_ref()
            .is_some_and(|launch| launch.kind != CoreKind::Mihomo)
            || (tun.backend == CoreRuntimeBackend::Service && ordinary_launch.is_none())
        {
            return Err(ServiceManagerError::Unsupported);
        }
        let request = ServiceMaintenanceRequest {
            operation,
            intent: tun.intent,
            capture_gate: self.session.capture_publication_gate(),
            backend: tun.backend,
            ordinary_launch,
        };
        self.check_maintenance_request(&request)?;
        Ok(request)
    }

    /// Leaves Service ownership before a separate native maintenance authorization.
    ///
    /// Existing Local ownership is preserved. Service recovery uses held bytes,
    /// disables TUN and retains its observations/resources in the returned value.
    /// A result whose `ready()` is false prohibits native maintenance. Foreground
    /// cancellation does not abandon already admitted runtime recovery.
    ///
    /// # Errors
    /// Rejects stale or foreign requests, shutdown, concurrent commands and
    /// failed runtime recovery. No administrator operation is submitted here.
    pub async fn prepare_maintenance(
        &self,
        request: ServiceMaintenanceRequest,
        store: &crate::ControlledConfigStore,
    ) -> Result<ServiceMaintenancePreparation, ServiceManagerError> {
        self.check_maintenance_request(&request)?;
        self.ensure_recovery_confirmed()?;
        let store = store.clone();
        self.complete(request.operation, move |manager| async move {
            manager.check_maintenance_request(&request)?;
            manager.ensure_recovery_confirmed()?;
            let recovery = if request.backend == CoreRuntimeBackend::Service {
                manager
                    .state
                    .send_modify(|state| state.phase = ServicePhase::Restoring);
                Some(Arc::new(
                    manager
                        .capture
                        .recover_service_to_local(
                            &manager.session,
                            &store,
                            request.ordinary_launch.clone(),
                            request.intent.binding,
                            request.intent.generation,
                        )
                        .await
                        .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))?,
                ))
            } else {
                None
            };
            let mut request = request;
            if let Some(recovery) = &recovery {
                request.intent = ServiceIntent {
                    binding: manager.session.runtime_descriptor().binding_generation(),
                    generation: recovery.runtime().generation(),
                };
                request.backend = CoreRuntimeBackend::Local;
            }
            manager.check_maintenance_request(&request)?;
            Ok(ServiceMaintenancePreparation {
                request,
                recovery,
                allow_authorization: false,
            })
        })
        .await
    }

    fn check_maintenance_request(
        &self,
        request: &ServiceMaintenanceRequest,
    ) -> Result<(), ServiceManagerError> {
        if !Arc::ptr_eq(
            &request.capture_gate,
            &self.session.capture_publication_gate(),
        ) {
            return Err(ServiceManagerError::Stale);
        }
        self.check_intent(request.intent)?;
        if self.session.runtime_descriptor().backend() != request.backend {
            return Err(ServiceManagerError::Stale);
        }
        if request.backend == CoreRuntimeBackend::Direct && !self.session.is_offline_recovery() {
            return Err(ServiceManagerError::Unsupported);
        }
        Ok(())
    }

    fn ensure_recovery_confirmed(&self) -> Result<(), ServiceManagerError> {
        let lifecycle = self.session.lifecycle_snapshot();
        if lifecycle.phase == crate::CoreLifecyclePhase::Unknown && lifecycle.stop_requested {
            return Err(ServiceManagerError::Runtime(Box::new(
                crate::MihomoError::Process(zenclash_i18n::text(
                    "core_page.service.cleanup_unconfirmed",
                ))
                .into(),
            )));
        }
        Ok(())
    }
}

fn maintenance_action(
    operation: ServiceOperation,
    health: &ServiceHealth,
) -> Result<Option<zenclash_service_integration::health::PendingAction>, ServiceManagerError> {
    use zenclash_service_integration::health::PendingAction;
    match (operation, health) {
        (ServiceOperation::Uninstall, ServiceHealth::NotInstalled) => Ok(None),
        (ServiceOperation::Repair, ServiceHealth::NotInstalled) => Ok(Some(PendingAction::Install)),
        (
            ServiceOperation::Repair,
            ServiceHealth::Ready | ServiceHealth::VersionMismatch | ServiceHealth::Unavailable(_),
        ) => Ok(Some(PendingAction::ForceReinstall)),
        (
            ServiceOperation::Uninstall,
            ServiceHealth::Ready | ServiceHealth::VersionMismatch | ServiceHealth::Unavailable(_),
        ) => Ok(Some(PendingAction::Uninstall)),
        (ServiceOperation::Repair | ServiceOperation::Uninstall, _) => {
            Err(ServiceManagerError::Health(health.clone()))
        }
        _ => Err(ServiceManagerError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_manager() -> ServiceManager {
        let home = std::env::temp_dir().join("maintenance-preparation-unused-home");
        let process = crate::MihomoProcess::prepare_stopped(crate::MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: home.join("ordinary-mihomo"),
            config_file: home.join("missing-source.yaml"),
            home_dir: home,
            endpoint: crate::MihomoEndpoint::default(),
            controller_override: None,
        });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::from_process(process).unwrap(),
        )
        .unwrap();
        let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
        ServiceManager::new(session, capture)
    }

    fn offline_manager(kind: CoreKind, binary: Option<PathBuf>) -> ServiceManager {
        let session = CoreSession::open_offline(
            kind,
            std::env::temp_dir().join("offline-maintenance-unused-home"),
            binary,
        )
        .unwrap();
        let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
        ServiceManager::new(session, capture)
    }

    #[tokio::test]
    async fn offline_maintenance_prepares_without_core_profile_or_capture_changes() {
        let manager = offline_manager(
            CoreKind::Mihomo,
            Some(std::env::temp_dir().join("missing-selected-mihomo")),
        );
        let before = manager.session.runtime_descriptor();
        let capture_revision = manager.capture.capture_revision();
        for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
            let request = manager.request_maintenance(operation, None).unwrap();
            let prepared = manager
                .prepare_maintenance(request, &unused_store())
                .await
                .unwrap();
            assert!(prepared.ready());
            assert!(prepared.recovery().is_none());
            assert!(prepared.request.ordinary_launch.is_none());
            assert_eq!(
                manager.session.runtime_descriptor().backend(),
                CoreRuntimeBackend::Direct
            );
            assert_eq!(
                manager.session.runtime_descriptor().binding_generation(),
                before.binding_generation()
            );
            assert_eq!(manager.session.generation(), 0);
            assert_eq!(manager.capture.capture_revision(), capture_revision);
            assert!(
                manager
                    .session
                    .committed_profile_snapshot()
                    .profile_path
                    .is_none()
            );
            assert!(matches!(
                manager.maintain_prepared(&prepared).await,
                Err(ServiceManagerError::ConsentRequired)
            ));
        }
        assert!(matches!(
            manager.request_enable_tun(),
            Err(ServiceManagerError::Unsupported)
        ));
    }

    #[test]
    fn external_controller_cannot_acquire_offline_maintenance_from_endpoint_text() {
        let client = crate::MihomoClient::new(crate::MihomoEndpoint::new(
            "127.0.0.1:0",
            "zenclash-offline",
        ))
        .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        assert!(!session.is_offline_recovery());
        let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
        let manager = ServiceManager::new(session, capture);
        for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
            assert!(matches!(
                manager.request_maintenance(operation, None),
                Err(ServiceManagerError::Unsupported)
            ));
        }
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    }

    #[tokio::test]
    async fn rebinding_offline_session_revokes_maintenance_even_for_the_same_endpoint() {
        let manager = offline_manager(CoreKind::Mihomo, None);
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        manager
            .session
            .switch_to_direct(crate::MihomoEndpoint::new(
                "127.0.0.1:0",
                "zenclash-offline",
            ))
            .await
            .unwrap();
        assert!(!manager.session.is_offline_recovery());
        assert!(matches!(
            manager
                .maintain_prepared(&prepared.with_authorization())
                .await,
            Err(ServiceManagerError::Stale)
        ));
        assert!(matches!(
            manager.request_maintenance(ServiceOperation::Repair, None),
            Err(ServiceManagerError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn offline_preparation_rejects_another_sessions_receipt_before_native_work() {
        let manager = offline_manager(CoreKind::Mihomo, None);
        let other = offline_manager(CoreKind::Mihomo, None);
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        assert!(matches!(
            other
                .maintain_prepared(&prepared.with_authorization())
                .await,
            Err(ServiceManagerError::Stale)
        ));
        assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
    }

    #[tokio::test]
    async fn offline_discovery_preserves_selected_source_without_parsing_a_profile() {
        let selected = std::env::temp_dir().join("offline-preferred-mihomo");
        let manager = offline_manager(CoreKind::Mihomo, Some(selected.clone()));
        let request = manager
            .discover_maintenance_request(
                ServiceOperation::Repair,
                PathBuf::from("missing-project"),
                Some(PathBuf::from("another-missing-core")),
            )
            .await
            .unwrap();
        assert!(request.ordinary_launch.is_none());
        assert_eq!(
            manager.session.offline_source().unwrap().binary.as_ref(),
            Some(&selected)
        );
        assert_eq!(manager.snapshot().revision(), 0);
    }

    #[tokio::test]
    async fn missing_selected_offline_core_is_rejected_before_authorization_without_fallback() {
        let selected = std::env::temp_dir().join("missing-offline-selected-core");
        let manager = offline_manager(CoreKind::Mihomo, Some(selected));
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        assert!(matches!(
            manager.repair_core_source(&request).await,
            Err(ServiceManagerError::Runtime(_))
        ));
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
        assert_eq!(manager.session.generation(), 0);
    }

    #[tokio::test]
    async fn selected_offline_core_is_verified_without_starting_it() {
        let source = std::env::temp_dir().join(format!(
            "zenclash-offline-source-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&source, b"not an executable and must not be started").unwrap();
        let manager = offline_manager(CoreKind::Mihomo, Some(source.clone()));
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        assert_eq!(manager.repair_core_source(&request).await.unwrap(), source);
        assert_eq!(
            manager.session.runtime_descriptor().backend(),
            CoreRuntimeBackend::Direct
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"not an executable and must not be started"
        );
        std::fs::remove_file(source).unwrap();
    }

    #[test]
    fn unknown_health_never_authorizes_maintenance_and_absent_uninstall_needs_no_prompt() {
        use zenclash_service_integration::health::PendingAction;
        assert_eq!(
            maintenance_action(ServiceOperation::Repair, &ServiceHealth::NotInstalled).unwrap(),
            Some(PendingAction::Install)
        );
        assert_eq!(
            maintenance_action(ServiceOperation::Uninstall, &ServiceHealth::NotInstalled).unwrap(),
            None
        );
        for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
            assert!(matches!(
                maintenance_action(operation, &ServiceHealth::Unknown),
                Err(ServiceManagerError::Health(ServiceHealth::Unknown))
            ));
        }
        for health in [
            ServiceHealth::Ready,
            ServiceHealth::VersionMismatch,
            ServiceHealth::Unavailable("stopped".into()),
        ] {
            assert_eq!(
                maintenance_action(ServiceOperation::Repair, &health).unwrap(),
                Some(PendingAction::ForceReinstall)
            );
        }
    }

    #[test]
    fn offline_maintenance_rejects_experimental_core_and_mismatched_capture_owner() {
        let experimental = offline_manager(CoreKind::Meow, None);
        assert!(matches!(
            experimental.request_maintenance(ServiceOperation::Repair, None),
            Err(ServiceManagerError::Unsupported)
        ));
        let manager = offline_manager(CoreKind::Mihomo, None);
        let other = offline_manager(CoreKind::Mihomo, None);
        let mismatched = ServiceManager::new(manager.session, other.capture);
        assert!(matches!(
            mismatched.request_maintenance(ServiceOperation::Repair, None),
            Err(ServiceManagerError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn offline_maintenance_rejects_shutdown_and_changed_generation() {
        let manager = offline_manager(CoreKind::Mihomo, None);
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        manager.session.mark_runtime_unknown();
        assert!(matches!(
            manager.prepare_maintenance(request, &unused_store()).await,
            Err(ServiceManagerError::Stale)
        ));
        manager.session.shutdown().await.unwrap();
        assert!(matches!(
            manager.request_maintenance(ServiceOperation::Repair, None),
            Err(ServiceManagerError::Closed)
        ));
    }

    #[test]
    fn offline_recovery_rejects_relative_source_paths() {
        assert!(
            CoreSession::open_offline(CoreKind::Mihomo, PathBuf::from("relative-home"), None)
                .is_err()
        );
        assert!(
            CoreSession::open_offline(
                CoreKind::Mihomo,
                std::env::temp_dir(),
                Some(PathBuf::from("relative-core"))
            )
            .is_err()
        );
    }

    fn unused_store() -> crate::ControlledConfigStore {
        crate::ControlledConfigStore::new(
            std::env::temp_dir().join("maintenance-preparation-unused-store"),
        )
    }

    #[tokio::test]
    async fn maintenance_capture_restore_preserves_preexisting_local_without_native_work() {
        let manager = local_manager();
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        let outcome = manager.restore_prepared_capture(&prepared).await.unwrap();
        assert!(outcome.tun().is_none());
        assert!(outcome.warning().is_none());
        assert_eq!(manager.session.generation(), 0);
        assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
    }

    #[tokio::test]
    async fn maintenance_capture_restore_rejects_uninstall_and_unconfirmed_native_result() {
        let manager = local_manager();
        let request = manager
            .request_maintenance(ServiceOperation::Uninstall, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        assert!(matches!(
            manager.restore_prepared_capture(&prepared).await,
            Err(ServiceManagerError::Unsupported)
        ));
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        manager
            .state
            .send_modify(|state| state.phase = ServicePhase::Unconfirmed);
        assert!(matches!(
            manager.restore_prepared_capture(&prepared).await,
            Err(ServiceManagerError::Busy)
        ));
        assert_eq!(manager.session.generation(), 0);
    }

    #[tokio::test]
    async fn maintenance_discovery_preserves_local_identity_without_reading_invalid_inputs() {
        let manager = local_manager();
        let before = manager.session.runtime_descriptor();
        for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
            let request = manager
                .discover_maintenance_request(
                    operation,
                    PathBuf::from("missing-project"),
                    Some(PathBuf::from("missing-binary")),
                )
                .await
                .unwrap();
            assert_eq!(request.operation(), operation);
            assert_eq!(
                request.ordinary_launch.unwrap().binary,
                before.binary().unwrap()
            );
            assert_eq!(manager.snapshot().revision(), 0);
            assert_eq!(manager.session.generation(), 0);
        }
        assert!(matches!(
            manager
                .discover_maintenance_request(
                    ServiceOperation::EnableTun,
                    PathBuf::from("missing-project"),
                    None,
                )
                .await,
            Err(ServiceManagerError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn service_maintenance_preparation_preserves_existing_local_without_source_reads() {
        let manager = local_manager();
        let before = manager.session.runtime_descriptor();
        for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
            let request = manager.request_maintenance(operation, None).unwrap();
            let prepared = manager
                .prepare_maintenance(request, &unused_store())
                .await
                .unwrap();
            assert!(prepared.ready());
            assert!(prepared.recovery().is_none());
            assert_eq!(prepared.operation(), operation);
            assert_eq!(
                manager.session.runtime_descriptor().binding_generation(),
                before.binding_generation()
            );
            assert_eq!(manager.session.generation(), 0);
            assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
        }
    }

    #[tokio::test]
    async fn service_maintenance_preparation_rejects_equal_versions_from_another_session() {
        let first = local_manager();
        let other = local_manager();
        let request = first
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        assert!(matches!(
            other.prepare_maintenance(request, &unused_store()).await,
            Err(ServiceManagerError::Stale)
        ));
        assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
    }

    #[tokio::test]
    async fn service_maintenance_submission_rejects_foreign_and_stale_preparation_before_native_work()
     {
        let manager = local_manager();
        let other = local_manager();
        let request = manager
            .request_maintenance(ServiceOperation::Repair, None)
            .unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        assert!(matches!(
            manager.maintain_prepared(&prepared).await,
            Err(ServiceManagerError::ConsentRequired)
        ));
        assert!(matches!(
            other.maintain_prepared(&prepared).await,
            Err(ServiceManagerError::Stale)
        ));
        assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
        manager.session.mark_runtime_unknown();
        assert!(matches!(
            manager.maintain_prepared(&prepared).await,
            Err(ServiceManagerError::Stale)
        ));
        assert_eq!(manager.snapshot().revision(), 1);
    }

    #[tokio::test]
    async fn service_maintenance_preparation_rejects_configuration_change_before_submission() {
        let manager = local_manager();
        let request = manager
            .request_maintenance(ServiceOperation::Uninstall, None)
            .unwrap();
        manager.session.mark_runtime_unknown();
        assert!(matches!(
            manager.prepare_maintenance(request, &unused_store()).await,
            Err(ServiceManagerError::Stale)
        ));
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    }

    #[test]
    fn service_maintenance_request_rejects_nonmaintenance_operations() {
        let manager = local_manager();
        for operation in [ServiceOperation::Refresh, ServiceOperation::EnableTun] {
            assert!(matches!(
                manager.request_maintenance(operation, None),
                Err(ServiceManagerError::Unsupported)
            ));
        }
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    }
}
