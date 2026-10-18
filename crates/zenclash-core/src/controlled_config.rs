//! Persistent, non-destructive configuration overrides for active profiles.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use serde_yaml::Value;
use thiserror::Error;

use crate::{
    CoreKind, ManagedCoreProcess, MihomoClient, MihomoError,
    data_coordinator::{DataWriteAccess, DataWriteLease, shared_transaction},
    listener_fallback::{
        SessionListenerFallback, apply_session_fallbacks, resolve_conflicts,
        validate_listener_change,
    },
    profile::{merge_payload_overrides, merge_profile_patch, merge_yaml},
    profiles::{MAX_PROFILE_BYTES, atomic_write, read_profile_bytes},
};

mod configuration_application;
mod partial_configuration_application;
mod service_configuration;
#[cfg(test)]
use partial_configuration_application::ModePatchGate;
use partial_configuration_application::contains_delta;
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

use runtime_application::{
    AcceptedRuntime, RuntimeMutationGuards, result_label, rollback_cache_and_restart,
    rollback_runtime_cache,
};
pub(crate) use runtime_application::{
    RuntimeApplicationTransaction, RuntimeCandidateValidation, RuntimeMutationError,
};
use runtime_normalization::normalize_partial_tun_payload;
pub(crate) use runtime_normalization::normalize_runtime_payload;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use storage::CommitGate;
use storage::{RuntimeCacheTransaction, require_mapping};

/// Receipt created only after durable business persistence, even if finalization fails.
pub(crate) struct SavedRuntimeReceipt {
    pub(crate) delta: Option<serde_json::Value>,
    pub(crate) confirmation: ControlledConfigResult<()>,
}

/// Errors produced while preparing or persisting controlled configuration.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ControlledConfigError {
    /// Filesystem access failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.io", &[("error", .0.to_string())])
    )]
    Io(#[from] std::io::Error),
    /// YAML parsing or serialization failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.yaml", &[("error", .0.to_string())])
    )]
    Yaml(#[from] serde_yaml::Error),
    /// JSON input could not be converted to YAML.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.json", &[("error", .0.to_string())])
    )]
    Json(#[from] serde_json::Error),
    /// A profile or merged payload was rejected before persistence.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.profile", &[("error", .0.to_string())])
    )]
    Profile(#[from] MihomoError),
    /// The override document was not a YAML mapping.
    #[error("{}", zenclash_i18n::text("controlled_config.errors.not_mapping"))]
    NotMapping,
    /// The override changed after an update was prepared.
    #[error(
        "{}",
        zenclash_i18n::text("controlled_config.errors.concurrent_modification")
    )]
    ConcurrentModification,
    /// An enabled YAML override would persist a different outbound mode.
    #[error("{}", zenclash_i18n::text_with("mode.errors.override_conflict", &[("requested", requested.clone()), ("effective", effective.clone())]))]
    ModeOverrideConflict {
        /// Outbound mode requested by the caller.
        requested: String,
        /// Outbound mode supplied by the enabled YAML override chain.
        effective: String,
    },
    /// An enabled override conflicts with a requested partial configuration field.
    #[error("{}", zenclash_i18n::text("overrides.errors.partial_conflict"))]
    PartialOverrideConflict,
    /// The override exceeded the defensive file-size limit.
    #[error("{}", zenclash_i18n::text("controlled_config.errors.too_large"))]
    TooLarge,
    /// A platform data directory could not be determined.
    #[error(
        "{}",
        zenclash_i18n::text("controlled_config.errors.missing_data_directory")
    )]
    MissingDataDirectory,
    /// An asynchronous preparation or commit worker ended unexpectedly.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.task", &[("error", .0.clone())])
    )]
    Task(String),
    /// Persistence failed after Mihomo accepted the update.
    #[error(
        "{}",
        zenclash_i18n::text_with("controlled_config.errors.transaction", &[("error", .0.clone())])
    )]
    Transaction(String),
    /// The selected runtime core cannot perform an operation in its current mode.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "controlled_config.errors.unsupported_core_operation",
            &[("error", .0.clone())]
        )
    )]
    UnsupportedCoreOperation(String),
    /// Listener probing or session-only fallback failed.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "controlled_config.errors.listener_fallback",
            &[("error", .0.clone())]
        )
    )]
    ListenerFallback(String),
}

/// Result type for controlled-configuration operations.
pub type ControlledConfigResult<T> = Result<T, ControlledConfigError>;

/// One listener port moved for the current managed-core session only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerPortFallback {
    /// Clash/Mihomo configuration key, such as `mixed-port`.
    pub listener: String,
    /// Port requested by the effective persistent configuration.
    pub original: u16,
    /// Available port selected for this process session.
    pub current: u16,
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
        Self {
            tun_projection: TunProjection::PreserveIntent,
            write_access: DataWriteAccess::new(&root),
            transaction: shared_transaction(&root),
            root,
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            session_listener_fallbacks: Arc::new(Mutex::new(BTreeMap::new())),
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
