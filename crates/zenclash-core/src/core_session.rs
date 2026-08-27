//! Serialized, intent-oriented access to runtime-core transitions.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use thiserror::Error;

use crate::{
    ControlledConfigError, ControlledConfigStore, CoreKind, MihomoClient, MihomoError,
    MihomoProcess,
};

const CORE_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Effective-configuration change requested by a runtime caller.
#[derive(Clone, Debug)]
pub enum EffectiveConfigIntent {
    /// Merge and persist a JSON patch over the active source profile.
    Patch {
        /// Active source profile.
        profile: PathBuf,
        /// Recursive JSON object patch.
        patch: serde_json::Value,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Apply a different profile without changing its source file.
    ActivateProfile {
        /// Candidate source profile.
        profile: PathBuf,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
}

/// Maintenance transition requested for a managed runtime core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreMaintenanceIntent {
    /// Restart the owned child and wait for its controller.
    Restart,
}

/// Mechanism that successfully applied an effective configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreApplyKind {
    /// The controller accepted a complete hot reload.
    HotReloaded,
    /// A managed child restarted with the generated runtime cache.
    Restarted,
}

/// Successful configuration transition and its monotonically increasing generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreApplyOutcome {
    /// Mechanism used for the accepted change.
    pub kind: CoreApplyKind,
    /// Session generation after the transition.
    pub generation: u64,
}

/// Point-in-time state exposed by [`CoreSession`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreSessionSnapshot {
    /// Concrete runtime core.
    pub kind: CoreKind,
    /// Whether ZenClash owns the core process.
    pub managed: bool,
    /// Whether the managed process is running, or the external session remains attached.
    pub running: bool,
    /// Number of successful runtime transitions.
    pub generation: u64,
}

/// Errors returned by intent-oriented core transitions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreSessionError {
    /// Effective configuration preparation, application, or persistence failed.
    #[error(transparent)]
    Config(#[from] ControlledConfigError),
    /// Managed process transition failed.
    #[error(transparent)]
    Process(#[from] MihomoError),
    /// An external core would require a process restart ZenClash does not own.
    #[error("外部 {core} 内核不支持需要重启的运行时变更")]
    ExternalRestartUnsupported {
        /// External runtime-core implementation.
        core: CoreKind,
    },
}

/// Cloneable owner of serialized effective-config and lifecycle transitions.
#[derive(Clone)]
pub struct CoreSession {
    kind: CoreKind,
    client: MihomoClient,
    process: Option<Arc<MihomoProcess>>,
    transition: Arc<tokio::sync::Mutex<()>>,
    generation: Arc<AtomicU64>,
}

impl CoreSession {
    /// Opens a session over one controller and its optional owned child process.
    #[must_use]
    pub fn open(kind: CoreKind, client: MihomoClient, process: Option<Arc<MihomoProcess>>) -> Self {
        debug_assert!(process
            .as_ref()
            .is_none_or(|process| process.kind() == kind));
        Self {
            kind,
            client,
            process,
            transition: Arc::new(tokio::sync::Mutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns the concrete runtime core.
    #[must_use]
    pub const fn kind(&self) -> CoreKind {
        self.kind
    }

    /// Returns the shared query client associated with the session.
    #[must_use]
    pub const fn client(&self) -> &MihomoClient {
        &self.client
    }

    /// Applies an effective-configuration intent through hot reload or managed restart.
    ///
    /// Explicit API rejection is returned directly. A managed core falls back
    /// to restart only after transport or response-decoding failure, where the
    /// hot-reload outcome is uncertain. External cores are never restarted.
    ///
    /// # Errors
    ///
    /// Returns preparation, validation, controller, persistence, or process errors.
    pub async fn apply(
        &self,
        store: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let _transition = self.transition.lock().await;
        let kind = match intent {
            EffectiveConfigIntent::Patch {
                profile,
                patch,
                overrides,
            } => self.apply_patch(store, profile, patch, overrides).await?,
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                self.activate_profile(store, profile, overrides).await?
            }
        };
        Ok(CoreApplyOutcome {
            kind,
            generation: self.next_generation(),
        })
    }

    /// Performs a serialized managed-core maintenance transition.
    ///
    /// # Errors
    ///
    /// Returns an error for external cores or when restart/readiness fails.
    pub async fn maintain(&self, intent: CoreMaintenanceIntent) -> Result<u64, CoreSessionError> {
        let _transition = self.transition.lock().await;
        let process = self
            .process
            .as_ref()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })?;
        match intent {
            CoreMaintenanceIntent::Restart => {
                process.restart_and_wait(CORE_READY_TIMEOUT).await?;
            }
        }
        Ok(self.next_generation())
    }

    /// Stops the managed child, or detaches from an external controller.
    ///
    /// # Errors
    ///
    /// Returns an error when an owned child cannot be stopped and reaped.
    pub async fn shutdown(&self) -> Result<(), CoreSessionError> {
        let _transition = self.transition.lock().await;
        if let Some(process) = &self.process {
            process.stop_async().await?;
            self.next_generation();
        }
        Ok(())
    }

    /// Captures ownership, running state, and successful transition generation.
    #[must_use]
    pub fn snapshot(&self) -> CoreSessionSnapshot {
        CoreSessionSnapshot {
            kind: self.kind,
            managed: self.process.is_some(),
            running: self
                .process
                .as_ref()
                .is_none_or(|process| process.is_running()),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    async fn apply_patch(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            let process = self.require_managed_restart()?;
            store
                .apply_json_update_with_restart(process, profile, &patch, overrides)
                .await?;
            return Ok(CoreApplyKind::Restarted);
        }

        match store
            .apply_json_update_with_overrides(&self.client, &profile, &patch, overrides.clone())
            .await
        {
            Ok(()) => Ok(CoreApplyKind::HotReloaded),
            Err(error) if should_restart_after_hot_reload(&error) && self.process.is_some() => {
                store
                    .apply_json_update_with_restart(
                        self.process.clone().expect("managed process checked"),
                        profile,
                        &patch,
                        overrides,
                    )
                    .await?;
                Ok(CoreApplyKind::Restarted)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn activate_profile(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            store
                .restart_with_overrides(self.require_managed_restart()?, profile, overrides)
                .await?;
            return Ok(CoreApplyKind::Restarted);
        }

        match store
            .reload_with_overrides(&self.client, &profile, overrides.clone())
            .await
        {
            Ok(()) => Ok(CoreApplyKind::HotReloaded),
            Err(error) if should_restart_after_hot_reload(&error) && self.process.is_some() => {
                store
                    .restart_with_overrides(
                        self.process.clone().expect("managed process checked"),
                        profile,
                        overrides,
                    )
                    .await?;
                Ok(CoreApplyKind::Restarted)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn require_managed_restart(&self) -> Result<Arc<MihomoProcess>, CoreSessionError> {
        self.process
            .clone()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
}

fn should_restart_after_hot_reload(error: &ControlledConfigError) -> bool {
    matches!(error, ControlledConfigError::Profile(MihomoError::Http(_)))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[tokio::test]
    async fn accepted_profile_apply_reports_hot_reload_and_advances_generation() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
            request
        });
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-apply-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client =
            MihomoClient::new(crate::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client, None);

        let outcome = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile,
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
        assert_eq!(outcome.generation, 1);
        assert_eq!(session.snapshot().generation, 1);
        assert!(request.starts_with("PUT /configs?force=true "));
        assert!(store.runtime_path().is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_meow_never_fakes_a_restart_capability() {
        let session = CoreSession::open(
            CoreKind::Meow,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
            None,
        );
        let store = ControlledConfigStore::new(std::env::temp_dir().join(format!(
            "zenclash-core-session-external-{}",
            std::process::id()
        )));

        let error = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: PathBuf::from("unused.yaml"),
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CoreSessionError::ExternalRestartUnsupported {
                core: CoreKind::Meow
            }
        ));
        assert_eq!(session.snapshot().generation, 0);
    }

    #[test]
    fn explicit_config_rejection_is_not_a_restart_signal() {
        let error = ControlledConfigError::Profile(MihomoError::Api {
            status: 400,
            message: "invalid config".into(),
        });

        assert!(!should_restart_after_hot_reload(&error));
    }
}
