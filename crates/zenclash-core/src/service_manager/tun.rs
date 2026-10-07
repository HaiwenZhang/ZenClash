use std::path::{Path, PathBuf};

use crate::service::{ServiceSession, health::PendingAction, maintenance::maintain_service};

use super::*;

impl ServiceManager {
    pub(crate) fn owns_profile_application(
        &self,
        session: &crate::CoreSession,
        store: &crate::ControlledConfigStore,
    ) -> bool {
        std::sync::Arc::ptr_eq(
            &self.session.capture_publication_gate(),
            &session.capture_publication_gate(),
        ) && self
            .capture
            .controlled_store()
            .is_some_and(|owned| owned.root() == store.root())
    }

    pub(crate) async fn apply_service_profile(
        &self,
        prepared: std::sync::Arc<crate::core_session::service_tun::PreparedServiceTun>,
    ) -> Result<crate::ServiceTunOutcome, ServiceManagerError> {
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            let mut request = manager.request_enable_tun()?.with_authorization();
            if prepared.expected_runtime() != (request.intent.binding, request.intent.generation) {
                return Err(ServiceManagerError::Stale);
            }
            manager.capture.note_capture_intent();
            request.prepared_config = Some(prepared);
            manager.enable_tun_admitted(request).await
        })
        .await
    }

    /// Applies a frozen Local configuration, authorizing service ownership for TUN.
    /// Returns `None` when the caller should use its ordinary configuration path.
    ///
    /// # Errors
    /// Rejects stale state, invalid candidates, unavailable service or native authorization.
    pub async fn try_apply_service_config(
        &self,
        store: &crate::ControlledConfigStore,
        intent: crate::EffectiveConfigIntent,
    ) -> Result<Option<ServiceConfigOutcome>, ServiceManagerError> {
        if self.session.kind() != CoreKind::Mihomo
            || self.session.runtime_descriptor().backend() != CoreRuntimeBackend::Local
        {
            return Ok(None);
        }
        let owned_store = self
            .capture
            .controlled_store()
            .ok_or(ServiceManagerError::Unsupported)?;
        if owned_store.root() != store.root() {
            return Err(ServiceManagerError::Unsupported);
        }
        let store = store.clone();
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            let mut request = manager.request_enable_tun()?.with_authorization();
            manager.capture.note_capture_intent();
            let _capture = manager
                .session
                .capture_publication_gate()
                .lock_owned()
                .await;
            let prepared = manager
                .session
                .prepare_service_config(
                    &store,
                    (request.intent.binding, request.intent.generation),
                    intent,
                )
                .await
                .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))?;
            drop(_capture);
            match prepared {
                crate::core_session::service_tun::PreparedConfig::Local(prepared) => manager
                    .session
                    .apply_prepared_local_config(&store, prepared)
                    .await
                    .map(|outcome| Some(ServiceConfigOutcome::Local(outcome)))
                    .map_err(|error| ServiceManagerError::Runtime(Box::new(error))),
                crate::core_session::service_tun::PreparedConfig::Service(prepared) => {
                    request.prepared_config = Some(prepared);
                    manager
                        .enable_tun_admitted(request)
                        .await
                        .map(|outcome| Some(ServiceConfigOutcome::Service(Box::new(outcome))))
                }
            }
        })
        .await
    }

    /// Installs or starts the approved helper if consented, then admits one TUN transaction.
    ///
    /// Native maintenance alone cannot produce a runtime receipt. The existing
    /// capture owner performs initial Start, accepted save, proxy coordination
    /// and recovery under its shared core publication gate.
    /// Local configuration and recovery resources are frozen before native
    /// authorization; changed configuration state rejects the prepared candidate.
    ///
    /// # Errors
    /// Rejects stale requests, shutdown, duplicate commands, missing consent,
    /// unavailable installation health, native maintenance or runtime failures.
    pub async fn enable_tun(
        &self,
        request: ServiceTunRequest,
    ) -> Result<crate::ServiceTunOutcome, ServiceManagerError> {
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            manager.capture.note_capture_intent();
            manager.enable_tun_admitted(request).await
        })
        .await
    }

    pub(super) async fn enable_tun_admitted(
        &self,
        mut request: ServiceTunRequest,
    ) -> Result<crate::ServiceTunOutcome, ServiceManagerError> {
        self.check_intent(request.intent)?;
        if request.backend == CoreRuntimeBackend::Local && request.prepared_config.is_none() {
            let store = self
                .capture
                .controlled_store()
                .ok_or(ServiceManagerError::Unsupported)?;
            let _capture = self.session.capture_publication_gate().lock_owned().await;
            self.check_intent(request.intent)?;
            request.prepared_config = Some(
                self.session
                    .prepare_service_tun(
                        &store,
                        request.intent.binding,
                        request.intent.generation,
                        self.capture.current_profile(),
                        request.recovery_bundle.clone(),
                    )
                    .await
                    .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))?,
            );
        }
        let service = if request.backend == CoreRuntimeBackend::Service {
            None
        } else {
            self.authorize_service_request(&request).await?;
            let source_home = request
                .source_home
                .as_ref()
                .ok_or(ServiceManagerError::Unsupported)?;
            self.check_intent(request.intent)?;
            let service = ServiceSession::connect(source_home)
                .await
                .map_err(ServiceManagerError::Connection)?;
            if let Err(error) = self.check_intent(request.intent) {
                service
                    .stop()
                    .await
                    .map_err(ServiceManagerError::Connection)?;
                return Err(error);
            }
            Some((Arc::new(service), source_home.clone()))
        };
        self.state
            .send_modify(|state| state.phase = ServicePhase::Switching);
        self.capture
            .enable_service_tun_with_bundle(
                service,
                request.intent.binding,
                request.intent.generation,
                request.recovery_bundle,
                request.expected_capture_revision,
                request.prepared_config,
            )
            .await
            .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))
    }

    pub(super) async fn authorize_service_request(
        &self,
        request: &ServiceTunRequest,
    ) -> Result<(), ServiceManagerError> {
        self.check_intent(request.intent)?;
        let health = self.observe_health().await;
        self.state
            .send_modify(|state| state.health = Some(Arc::new(health.clone())));
        self.check_intent(request.intent)?;
        let action = match health {
            ServiceHealth::Ready => None,
            ServiceHealth::NotInstalled => Some(PendingAction::Install),
            ServiceHealth::VersionMismatch | ServiceHealth::Unavailable(_) => {
                Some(PendingAction::Reinstall)
            }
            ServiceHealth::Unknown => return Err(ServiceManagerError::Health(health)),
        };
        if let Some(action) = action {
            if !request.allow_authorization {
                return Err(ServiceManagerError::ConsentRequired);
            }
            let helper = bundled_helper().await?;
            let core = request
                .core_source
                .as_ref()
                .ok_or(ServiceManagerError::Unsupported)?;
            self.authorize_action(
                request.intent,
                action,
                maintain_service(action, helper, Some(core.clone())),
            )
            .await?;
            let health = self.session.run_state().health;
            self.state
                .send_modify(|state| state.health = Some(Arc::new(health.clone())));
            if health != ServiceHealth::Ready {
                return Err(ServiceManagerError::Health(health));
            }
        }
        self.check_intent(request.intent)
    }
}

pub(super) async fn bundled_helper() -> Result<PathBuf, ServiceManagerError> {
    tokio::task::spawn_blocking(|| {
        let executable = std::env::current_exe()?;
        helper_for_executable(&executable)
    })
    .await
    .map_err(ServiceManagerError::Completion)?
    .map_err(ServiceManagerError::Sources)
}

fn helper_for_executable(executable: &Path) -> std::io::Result<PathBuf> {
    if !executable.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "application executable path is not absolute",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        let directory = executable.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "application directory missing",
            )
        })?;
        Ok(directory.join("zenclash-service-install.exe"))
    }
    #[cfg(target_os = "macos")]
    {
        let directory = executable.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "application directory missing",
            )
        })?;
        let contents = directory.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "bundle contents missing")
        })?;
        if directory.file_name() != Some(std::ffi::OsStr::new("MacOS"))
            || contents.file_name() != Some(std::ffi::OsStr::new("Contents"))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "application service requires its installed bundle",
            ));
        }
        Ok(directory.join("zenclash-service-install"))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/usr/lib/zenclash/zenclash-service-install"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn directory_service_candidate_rejects_new_generation_before_native_health() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-directory-manager-stale",
        )
        .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = crate::ControlledConfigStore::new(home.join("controlled"));
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(store.runtime_path(), "tun: {enable: false}\n").unwrap();
        let manager = ServiceManager::new(
            session.clone(),
            TrafficCaptureSession::new(session.clone(), store.clone(), None, None),
        );
        let prepared = session
            .prepare_service_profile_config(
                &store,
                (
                    session.runtime_descriptor().binding_generation(),
                    session.generation(),
                ),
                home.join("profiles/new.yaml"),
                std::sync::Arc::from("tun: {enable: true}\n"),
                vec![],
            )
            .await
            .unwrap();
        let crate::core_session::service_tun::PreparedConfig::Service(prepared) = prepared else {
            panic!("TUN candidate requires service")
        };
        let pid = fixture.process.snapshot().pid;
        session.mark_runtime_unknown();
        assert!(matches!(
            manager.apply_service_profile(prepared).await,
            Err(ServiceManagerError::Stale)
        ));
        assert!(manager.snapshot().health().is_none());
        assert_eq!(manager.snapshot().phase(), ServicePhase::Failed);
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert_eq!(
            std::fs::read_to_string(store.runtime_path()).unwrap(),
            "tun: {enable: false}\n"
        );
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn service_config_applies_ordinary_candidate_without_native_health_or_authorization() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-config-local-apply")
                .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let store = crate::ControlledConfigStore::new(
            fixture.process.launch_config().home_dir.join("controlled"),
        );
        let manager = ServiceManager::new(
            session.clone(),
            TrafficCaptureSession::new(session.clone(), store.clone(), None, None),
        );
        let pid = fixture.process.snapshot().pid;
        let outcome = manager
            .try_apply_service_config(
                &store,
                crate::EffectiveConfigIntent::ActivateProfile {
                    profile: fixture.process.launch_config().config_file.clone(),
                    overrides: vec![],
                },
            )
            .await
            .unwrap()
            .unwrap();
        let ServiceConfigOutcome::Local(outcome) = outcome else {
            panic!("ordinary configuration requested service ownership")
        };
        assert_eq!(outcome.generation, 1);
        assert!(store.cached_runtime_payload().unwrap().is_some());
        assert!(manager.snapshot().health().is_none());
        assert_eq!(fixture.process.snapshot().pid, pid);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn service_config_rejects_another_store_before_health_or_authorization() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-config-store-identity",
        )
        .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let owned = crate::ControlledConfigStore::new(home.join("owned"));
        let other = crate::ControlledConfigStore::new(home.join("other"));
        let manager = ServiceManager::new(
            session.clone(),
            TrafficCaptureSession::new(session.clone(), owned.clone(), None, None),
        );
        let pid = fixture.process.snapshot().pid;
        assert!(matches!(
            manager
                .try_apply_service_config(
                    &other,
                    crate::EffectiveConfigIntent::ActivateProfile {
                        profile: fixture.process.launch_config().config_file.clone(),
                        overrides: vec![],
                    }
                )
                .await,
            Err(ServiceManagerError::Unsupported)
        ));
        assert!(manager.snapshot().health().is_none());
        assert!(!owned.root().exists());
        assert!(!other.root().exists());
        assert_eq!(fixture.process.snapshot().pid, pid);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_tun_candidate_is_rejected_before_native_health_or_authorization() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-tun-preflight-rejection",
        )
        .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let store = crate::ControlledConfigStore::new(
            fixture.process.launch_config().home_dir.join("controlled"),
        );
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(
            store.runtime_path(),
            "proxy-providers:\n  missing:\n    type: file\n    path: absent.yaml\n",
        )
        .unwrap();
        let capture = TrafficCaptureSession::new(
            session.clone(),
            store,
            None,
            Some(fixture.process.launch_config().config_file.clone()),
        );
        let manager = ServiceManager::new(session.clone(), capture);
        let pid = fixture.process.snapshot().pid;
        let request = manager.request_enable_tun().unwrap().with_authorization();
        assert!(matches!(
            manager.enable_tun(request).await,
            Err(ServiceManagerError::Runtime(_))
        ));
        assert!(manager.snapshot().health().is_none());
        assert_eq!(manager.snapshot().phase(), ServicePhase::Failed);
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert_eq!(session.generation(), 0);
        session.shutdown().await.unwrap();
    }
}
