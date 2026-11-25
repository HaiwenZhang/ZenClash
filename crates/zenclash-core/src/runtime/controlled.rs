//! Persistent, non-destructive configuration overrides for active profiles.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
};

use parking_lot::Mutex;
use serde_yaml::Value;

use crate::{
    CoreKind, ManagedCoreProcess, MihomoClient, MihomoError,
    mihomo::config::listener_fallback::{
        SessionListenerFallback, apply_session_fallbacks, apply_session_fallbacks_to_value,
        resolve_candidate_conflicts, resolve_conflicts, validate_listener_change,
    },
    mihomo::config::merge::{
        merge_controlled_yaml, merge_payload_overrides, merge_profile_patch, merge_yaml,
    },
    platform::data_coordinator::{DataWriteAccess, DataWriteLease, shared_transaction},
    storage::profiles::{MAX_PROFILE_BYTES, atomic_write, read_profile_bytes},
};

mod configuration_application;
mod partial_configuration_application;
mod service_configuration;
#[cfg(test)]
use partial_configuration_application::ModePatchGate;
use partial_configuration_application::ensure_delta_compatible_with_overrides;
pub(crate) use partial_configuration_application::{merge_held_delta, service_partial_patch};
pub(crate) use service_configuration::{ServiceStartupPersistence, ServiceTunPersistence};
mod exact_runtime_restore;
mod restart_application;
mod runtime_application;
use restart_application::AdmittedManagedRestart;
mod runtime_normalization;
mod runtime_projection;
mod source_composition;
mod storage;
use runtime_projection::TunProjection;

pub(crate) use crate::mihomo::config::pipeline::{
    normalize_runtime_payload, normalize_runtime_value,
};
use runtime_application::{
    AcceptedRuntime, RuntimeMutationGuards, result_label, rollback_cache_and_restart,
    rollback_runtime_cache,
};
pub(crate) use runtime_application::{
    RuntimeApplicationTransaction, RuntimeCandidateValidation, RuntimeMutationError,
};
use runtime_normalization::normalize_partial_tun_payload;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use storage::CommitGate;
use storage::{RuntimeCacheTransaction, require_mapping};

pub(crate) fn should_restart_after_hot_reload(error: &ControlledConfigError) -> bool {
    matches!(
        error,
        ControlledConfigError::Profile(
            MihomoError::Http(_)
                | MihomoError::ResponseDecode {
                    outcome_unknown: true,
                    ..
                }
        )
    )
}

/// Receipt created only after durable business persistence, even if finalization fails.
pub(crate) struct SavedRuntimeReceipt {
    pub(crate) delta: Option<serde_json::Value>,
    pub(crate) confirmation: ControlledConfigResult<()>,
}

pub use crate::mihomo::config::listener_fallback::ListenerPortFallback;
pub use crate::mihomo::errors::{ControlledConfigError, ControlledConfigResult};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BackupRuntimeCandidate {
    pub(crate) data_root: PathBuf,
    pub(crate) profile: PathBuf,
    pub(crate) overrides: Vec<PathBuf>,
    pub(crate) payload: String,
    pub(crate) patch: Vec<u8>,
}

/// Prepared controlled-config mutation that has not been persisted yet.
///
/// The new effective payload should first be accepted by Mihomo. Call
/// [`ControlledConfigStore::commit`] only after that succeeds. If persistence
/// then fails, reload [`Self::previous_payload`] to restore runtime state.
#[derive(Clone, Debug)]
pub struct ControlledConfigUpdate {
    expected_patch: Option<Vec<u8>>,
    next_patch: Vec<u8>,
    previous_payload: String,
    next_payload: String,
    next_session_fallbacks: Option<BTreeMap<String, SessionListenerFallback>>,
}

impl ControlledConfigUpdate {
    /// Effective YAML before this mutation.
    #[must_use]
    pub fn previous_payload(&self) -> &str {
        &self.previous_payload
    }

    /// Effective YAML including the proposed mutation.
    #[must_use]
    pub fn next_payload(&self) -> &str {
        &self.next_payload
    }
}

/// Shared controlled-config data and compatibility runtime-application entry points.
///
/// Subscription and local source files remain untouched. The stored mapping is
/// recursively merged over whichever profile is active. Runtime applications
/// share the original data and mutation authority; CoreSession owns lifecycle
/// transitions and publishes accepted configuration generations.
#[derive(Clone, Debug)]
pub struct ControlledConfigStore {
    tun_projection: TunProjection,
    root: PathBuf,
    write_access: DataWriteAccess,
    transaction: Arc<Mutex<()>>,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    session_listener_fallbacks: Arc<Mutex<BTreeMap<String, SessionListenerFallback>>>,
    #[cfg(test)]
    commit_gate: Option<Arc<CommitGate>>,
    #[cfg(test)]
    mode_patch_gate: Option<Arc<ModePatchGate>>,
}

impl ControlledConfigStore {
    /// Opens the platform-default `ZenClash` controlled-config directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the application data directory cannot be found.
    pub fn discover() -> ControlledConfigResult<Self> {
        let path = crate::AppDataPaths::discover()
            .ok_or(ControlledConfigError::MissingDataDirectory)?
            .controlled_config();
        Ok(Self::new(path))
    }

    /// Creates a store rooted at an explicit directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let paths = crate::platform::data_coordinator::coordinated_paths(&root);
        Self {
            tun_projection: TunProjection::PreserveIntent,
            write_access: DataWriteAccess::new(&root),
            transaction: shared_transaction(&root),
            mutation_gate: shared_mutation_gate(&paths),
            session_listener_fallbacks: shared_session_listener_fallbacks(&paths),
            root,
            #[cfg(test)]
            commit_gate: None,
            #[cfg(test)]
            mode_patch_gate: None,
        }
    }

    pub(crate) async fn acquire_write_lease(&self) -> ControlledConfigResult<DataWriteLease> {
        self.write_access
            .acquire_async()
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
    }

    pub(crate) async fn acquire_write_lease_for_paths(
        &self,
        paths: Vec<PathBuf>,
    ) -> ControlledConfigResult<DataWriteLease> {
        if paths.is_empty() {
            return self.acquire_write_lease().await;
        }
        self.write_access
            .acquire_paths_async(paths)
            .await
            .map_err(ControlledConfigError::Task)
    }

    pub(crate) fn with_write_lease(&self, lease: &DataWriteLease) -> Self {
        Self {
            write_access: self.write_access.authorized(lease),
            ..self.clone()
        }
    }
}

fn shared_mutation_gate(paths: &[PathBuf; 2]) -> Arc<tokio::sync::Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES.get_or_init(Mutex::default).lock();
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = paths
        .iter()
        .find_map(|path| gates.get(path).and_then(Weak::upgrade))
        .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
    for path in paths {
        gates.insert(path.clone(), Arc::downgrade(&gate));
    }
    gate
}

fn shared_session_listener_fallbacks(
    paths: &[PathBuf; 2],
) -> Arc<Mutex<BTreeMap<String, SessionListenerFallback>>> {
    type FallbackMap = Mutex<BTreeMap<String, SessionListenerFallback>>;
    static FALLBACKS: OnceLock<Mutex<HashMap<PathBuf, Weak<FallbackMap>>>> = OnceLock::new();
    let mut fallbacks = FALLBACKS.get_or_init(Mutex::default).lock();
    fallbacks.retain(|_, entry| entry.strong_count() > 0);
    let entry = paths
        .iter()
        .find_map(|path| fallbacks.get(path).and_then(Weak::upgrade))
        .unwrap_or_else(|| Arc::new(Mutex::new(BTreeMap::new())));
    for path in paths {
        fallbacks.insert(path.clone(), Arc::downgrade(&entry));
    }
    entry
}
