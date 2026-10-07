//! Read-only upstream health classification for the explicitly selected native core.

use super::*;
use crate::service::runstate::{NativeEnv, RunState, RunStateHost, RunStateStore};

struct ObservationHost;
impl RunStateHost for ObservationHost {
    // This store is used only for health detection. Core/PAC publication stays in CoreSession.
    fn set_pac_available(&self, _available: bool) {}
    fn publish(&self, _state: &RunState) {}
    fn run_privileged(&self, _action: crate::service::health::PendingAction) -> anyhow::Result<()> {
        anyhow::bail!("a read-only health probe cannot request native authorization")
    }
}

pub(super) async fn observe(home: PathBuf, binary: Option<PathBuf>) -> ServiceHealth {
    let selected = tokio::task::spawn_blocking(move || {
        let core = binary.map_or_else(|| crate::process::service_core_source(&home), Ok)?;
        let name = core
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                crate::MihomoError::InvalidInput("Selected core has no file name".into())
            })?
            .to_owned();
        let digest = zenclash_service::management::sha256_file(&core)
            .map_err(|error| crate::MihomoError::Process(error.to_string()))?;
        Ok::<_, crate::MihomoError>(zenclash_service::CoreRequirement {
            name,
            sha256: Some(digest),
        })
    })
    .await;
    let requirement = match selected {
        Ok(Ok(requirement)) => requirement,
        // Absence remains observable even if prebuild has not supplied a local core yet.
        error => {
            let registered =
                tokio::task::spawn_blocking(crate::service::platform::trusted_service_evidence)
                    .await;
            return if matches!(registered, Ok(Ok(false))) {
                ServiceHealth::NotInstalled
            } else {
                ServiceHealth::Unavailable(format!(
                    "selected core could not be verified: {error:?}"
                ))
            };
        }
    };
    RunStateStore::new(NativeEnv::new(requirement, ObservationHost))
        .detect_service_health()
        .await
}

impl ServiceManager {
    pub(super) async fn observe_health(&self) -> ServiceHealth {
        self.session.observe_service_health().await
    }
}
