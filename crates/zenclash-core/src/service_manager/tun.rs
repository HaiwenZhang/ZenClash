use std::path::{Path, PathBuf};

use zenclash_service::{MaintenanceAction, ServiceClient, maintain_service};

use super::*;

impl ServiceManager {
    /// Installs or starts the approved helper if consented, then admits one TUN transaction.
    ///
    /// Native maintenance alone cannot produce a runtime receipt. The existing
    /// capture owner performs initial Start, accepted save, proxy coordination
    /// and recovery under its shared core publication gate.
    ///
    /// # Errors
    /// Rejects stale requests, shutdown, duplicate commands, missing consent,
    /// unavailable installation health, native maintenance or runtime failures.
    pub async fn enable_tun(
        &self,
        request: ServiceTunRequest,
    ) -> Result<crate::ServiceTunOutcome, ServiceManagerError> {
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            manager.check_intent(request.intent)?;
            let service = if request.backend == CoreRuntimeBackend::Service {
                None
            } else {
                let source_home = request
                    .source_home
                    .as_ref()
                    .ok_or(ServiceManagerError::Unsupported)?;
                let health = service_health().await;
                manager
                    .state
                    .send_modify(|state| state.health = Some(Arc::new(health.clone())));
                manager.check_intent(request.intent)?;
                let action = match health.kind() {
                    ServiceHealthKind::Ready => None,
                    ServiceHealthKind::Missing => Some(MaintenanceAction::Install),
                    ServiceHealthKind::Stopped => Some(MaintenanceAction::Start),
                    kind => return Err(ServiceManagerError::Health(kind)),
                };
                if let Some(action) = action {
                    if !request.allow_authorization {
                        return Err(ServiceManagerError::ConsentRequired);
                    }
                    let helper = bundled_helper().await?;
                    let core = request
                        .core_source
                        .clone()
                        .ok_or(ServiceManagerError::Unsupported)?;
                    manager
                        .authorize_then(
                            request.intent,
                            maintain_service(action, &helper, Some(&core)),
                            || async { Ok(()) },
                        )
                        .await?;
                    let health = service_health().await;
                    manager
                        .state
                        .send_modify(|state| state.health = Some(Arc::new(health.clone())));
                    if health.kind() != ServiceHealthKind::Ready {
                        return Err(ServiceManagerError::Health(health.kind()));
                    }
                }
                manager.check_intent(request.intent)?;
                let service = ServiceClient::connect()
                    .await
                    .map_err(ServiceManagerError::Connection)?;
                if let Err(error) = manager.check_intent(request.intent) {
                    service
                        .release()
                        .await
                        .map_err(ServiceManagerError::Connection)?;
                    return Err(error);
                }
                Some((service, source_home.clone()))
            };
            manager
                .state
                .send_modify(|state| state.phase = ServicePhase::Switching);
            manager
                .capture
                .enable_service_tun(service, request.intent.binding, request.intent.generation)
                .await
                .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))
        })
        .await
    }
}

async fn bundled_helper() -> Result<PathBuf, ServiceManagerError> {
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
        Ok(directory.join("zenclash-service.exe"))
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
        Ok(directory.join("zenclash-service"))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/usr/lib/zenclash/zenclash-service"))
    }
}
