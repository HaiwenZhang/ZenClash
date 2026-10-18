//! Transaction owner for applying managed profiles to the runtime core.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

mod service_commit;
mod source_preparation;
mod staging;
pub(crate) use service_commit::ServiceProfileCommit;
use staging::{CommittedProfile, ProfileSourceCandidate};
#[cfg(test)]
mod tests;

use super::{
    MAX_PROFILE_BYTES, ProfileCatalog, ProfileRecord, ProfileSource, ProfileStore,
    ProfileStoreError, ProfileStoreResult, RemoteProfileOptions, RemoteProfileRoute,
    SubscriptionMetadata, atomic_write, download_profile, normalized_profile_name,
    normalized_remote_url, normalized_user_agent, read_profile_bytes, validate_clash_yaml,
};
use crate::{
    ControlledConfigStore, CoreApplyKind, CoreSession, CoreSessionError, MihomoError,
    core_session::{CoreProfileRollbackOutcome, CoreProfileStageError},
};

/// Managed-source metadata for reporting and recovery context, not content identity.
/// Different payloads can share the same timestamp and size; source conflicts
/// are checked against the complete catalog and retained bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileSourceMetadata {
    /// Stable managed-profile identifier.
    pub profile_id: String,
    /// Last source update timestamp recorded by the profile store.
    pub updated_at: u64,
    /// Persisted source payload size.
    pub size_bytes: u64,
}

impl ProfileSourceMetadata {
    /// Creates metadata without claiming a unique revision or runtime authority.
    #[must_use]
    pub fn new(profile_id: String, updated_at: u64, size_bytes: u64) -> Self {
        Self {
            profile_id,
            updated_at,
            size_bytes,
        }
    }
}

/// Compatibility name for managed-source metadata; this is not a unique version.
pub type ProfileVersion = ProfileSourceMetadata;

impl From<&ProfileRecord> for ProfileSourceMetadata {
    fn from(record: &ProfileRecord) -> Self {
        Self {
            profile_id: record.id.clone(),
            updated_at: record.updated_at,
            size_bytes: record.size_bytes,
        }
    }
}

/// A user intent handled by [`ProfileApplication`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileChange {
    /// Import a local YAML source and activate its managed copy.
    ImportLocal {
        /// User-selected local YAML source.
        source: PathBuf,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Download a remote subscription and activate its managed copy.
    AddRemote {
        /// User-facing profile name.
        name: String,
        /// HTTP(S) subscription endpoint.
        url: String,
        /// User-Agent sent to the subscription service.
        user_agent: String,
        /// Validated download and route policy.
        options: RemoteProfileOptions,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Refresh a remote profile and apply it when it is currently active.
    UpdateRemote {
        /// Stable managed-profile identifier.
        id: String,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Replace a managed YAML source after checking the editor base revision.
    EditYaml {
        /// Stable managed-profile identifier.
        id: String,
        /// Source payload loaded when the editor was opened.
        expected_payload: String,
        /// Candidate source payload entered by the user.
        new_payload: String,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Activate an existing managed profile with ordered YAML overrides.
    ActivateExisting {
        /// Stable managed-profile identifier.
        id: String,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
}

/// Recovery context returned when persistence and runtime truth may disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRecovery {
    /// Managed-source metadata whose application was attempted.
    pub attempted: ProfileSourceMetadata,
    /// Metadata of the source active before the attempt, when known.
    pub last_known_good: Option<ProfileSourceMetadata>,
}

/// Typed failure produced while preparing, applying, or rolling back a profile.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileApplicationError {
    /// The managed profile repository rejected an operation.
    #[error(transparent)]
    Store(#[from] ProfileStoreError),
    /// The effective configuration or runtime core rejected the candidate.
    #[error(transparent)]
    Runtime(#[from] CoreSessionError),
    /// A controller read required to prepare the transaction failed.
    #[error(transparent)]
    Controller(#[from] MihomoError),
    /// Service authorization or admission rejected the managed application.
    #[error(transparent)]
    Service(#[from] Box<crate::ServiceManagerError>),
    /// A blocking repository task ended unexpectedly.
    #[error(
        "{}",
        zenclash_i18n::text_with("profiles.errors.task", &[("error", .0.clone())])
    )]
    Task(String),
}

/// Rejection while preparing a source candidate, before runtime state changes.
#[derive(Debug, Error)]
#[error("{cause}")]
pub struct ProfilePreparationError {
    /// Last active managed source observed during preparation.
    pub last_known_good: Option<ProfileSourceMetadata>,
    /// Source, download, catalog or staging failure.
    #[source]
    pub cause: ProfileApplicationError,
}

impl From<ProfilePreparationError> for ProfileApplyOutcome {
    fn from(error: ProfilePreparationError) -> Self {
        Self::Rejected {
            last_known_good: error.last_known_good,
            cause: error.cause,
        }
    }
}

/// Result of one profile application transaction.
#[derive(Debug)]
pub enum ProfileApplyOutcome {
    /// The source was selected and the runtime accepted the effective candidate.
    Applied {
        /// Applied managed-profile metadata.
        profile: ProfileRecord,
        /// Managed source path used to build the effective configuration.
        path: PathBuf,
        /// Source metadata that was applied; not a unique content identity.
        source_version: ProfileSourceMetadata,
        /// Core-session generation after the successful transition.
        runtime_version: u64,
        /// Runtime mechanism used for the transition.
        kind: CoreApplyKind,
    },
    /// An inactive source was validated and committed without changing runtime.
    Stored {
        /// Updated managed-profile metadata.
        profile: ProfileRecord,
        /// Managed source path containing the committed revision.
        path: PathBuf,
        /// Source metadata that was committed; not a unique content identity.
        source_version: ProfileSourceMetadata,
    },
    /// The change was rejected before runtime state changed.
    Rejected {
        /// Last active source metadata observed before the attempt.
        last_known_good: Option<ProfileSourceMetadata>,
        /// Typed rejection cause.
        cause: ProfileApplicationError,
    },
    /// Persistence failed after runtime acceptance, and the prior runtime was restored.
    RolledBack {
        /// Restored source metadata, when one was active previously.
        last_known_good: Option<ProfileSourceMetadata>,
        /// Typed persistence failure that triggered runtime rollback.
        cause: ProfileApplicationError,
        /// Core-session generation of the completed runtime restoration.
        runtime_version: u64,
    },
    /// The source was not committed, but a transport failure obscured runtime truth.
    RuntimeUnknown {
        /// Candidate and last-known-good revisions needed for reconciliation.
        recovery: ProfileRecovery,
        /// Transport or recovery failure that made runtime state uncertain.
        cause: ProfileApplicationError,
        /// Core-session generation that invalidated observations of the uncertain runtime.
        runtime_version: u64,
    },
    /// Persistence may be partial and runtime rollback also failed.
    PersistedButRuntimeUnknown {
        /// Information required to offer a targeted recovery action.
        recovery: ProfileRecovery,
        /// Persistence failure that triggered the rollback attempt.
        cause: ProfileApplicationError,
        /// Runtime recovery failure that left effective state uncertain.
        rollback: ProfileApplicationError,
        /// Core-session generation of the completed, unsuccessful runtime restoration.
        runtime_version: u64,
    },
    /// The source and startup cache are committed, but service finalization is unconfirmed.
    /// Recovery must confirm the pending revision; it must not roll back durable user data.
    CommittedButRuntimeUnknown {
        /// Durable managed-profile metadata.
        profile: ProfileRecord,
        /// Durable source path.
        path: PathBuf,
        /// Source metadata saved before the finalization failure.
        source_version: ProfileSourceMetadata,
        /// Service finalization or status failure.
        cause: ProfileApplicationError,
        /// Generation describing the committed source identity.
        runtime_version: u64,
    },
}

/// Cloneable owner of managed-profile and runtime application ordering.
#[derive(Clone)]
pub struct ProfileApplication {
    store: ProfileStore,
    controlled: ControlledConfigStore,
    session: CoreSession,
    #[cfg(test)]
    commit_gate: Option<std::sync::Arc<crate::controlled_config::CommitGate>>,
}

/// Held managed-source candidate bound to one application session and repository.
/// Preparing it does not apply configuration or authorize service installation.
/// Source bytes and comparison metadata remain in memory; no staging file or write
/// authority is retained while callers wait for authorization.
pub struct PreparedProfileChange {
    source_candidate: ProfileSourceCandidate,
    overrides: Vec<PathBuf>,
    expected_runtime: crate::core_session::RuntimeConfigurationRevision,
    session: CoreSession,
    controlled_root: PathBuf,
    runtime: Option<crate::core_session::service_tun::PreparedConfig>,
}

impl PreparedProfileChange {
    /// Reports whether the checked Local/Mihomo candidate requires service ownership.
    /// This reports preparation only; it does not grant authorization or apply TUN.
    #[must_use]
    pub fn requires_service(&self) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.requires_service())
    }
}

impl ProfileApplication {
    /// Creates an application transaction owner over existing core services.
    #[must_use]
    pub fn new(
        store: ProfileStore,
        controlled: ControlledConfigStore,
        session: CoreSession,
    ) -> Self {
        Self {
            store,
            controlled,
            session,
            #[cfg(test)]
            commit_gate: None,
        }
    }

    /// Prepares held source bytes and catalog comparisons without changing runtime.
    /// No write lease, transition or runtime mutation gate remains held on return.
    /// Consume the candidate with this same application's `apply_prepared` after waiting.
    ///
    /// # Errors
    /// Returns a typed rejection for source, download, catalog or staging failures.
    pub async fn prepare_change(
        &self,
        change: ProfileChange,
    ) -> Result<PreparedProfileChange, ProfilePreparationError> {
        let expected_runtime = self.session.runtime_revision();
        let mut scopes = self.session.write_scopes();
        scopes.extend([
            self.store.root().to_path_buf(),
            self.controlled.root().to_path_buf(),
        ]);
        let lease = self
            .store
            .write_access
            .acquire_paths_async(scopes)
            .await
            .map_err(|error| ProfilePreparationError {
                last_known_good: None,
                cause: ProfileApplicationError::Task(error),
            })?;
        let application = Self {
            store: self.store.with_write_lease(&lease),
            controlled: self.controlled.with_write_lease(&lease),
            session: self.session.clone(),
            #[cfg(test)]
            commit_gate: self.commit_gate.clone(),
        };
        let (source_candidate, overrides) = application.stage_change(change).await?;
        let last_known_good = source_candidate.last_known_good();
        let runtime = if source_candidate.changes_runtime()
            && self.session.kind() == crate::CoreKind::Mihomo
            && self.session.runtime_descriptor().backend() == crate::CoreRuntimeBackend::Local
        {
            Some(
                self.session
                    .prepare_service_profile_config(
                        &application.controlled,
                        expected_runtime,
                        source_candidate
                            .store()
                            .profile_path(source_candidate.record()),
                        source_candidate.payload().clone(),
                        overrides.clone(),
                    )
                    .await
                    .map_err(|error| ProfilePreparationError {
                        last_known_good: last_known_good.clone(),
                        cause: error.into(),
                    })?,
            )
        } else {
            None
        };
        let ordinary_store = self.store.clone();
        let source_candidate =
            run_store(move || source_candidate.into_held_candidate(ordinary_store))
                .await
                .map_err(|cause| ProfilePreparationError {
                    last_known_good,
                    cause,
                })?;
        Ok(PreparedProfileChange {
            source_candidate,
            overrides,
            expected_runtime,
            session: self.session.clone(),
            controlled_root: self.controlled.root().to_path_buf(),
            runtime,
        })
    }

    /// Applies one managed-profile change and classifies its recovery state.
    pub async fn apply(&self, change: ProfileChange) -> ProfileApplyOutcome {
        match self.prepare_change(change).await {
            Ok(prepared) => self.apply_prepared(prepared).await,
            Err(error) => error.into(),
        }
    }

    /// Consumes one held candidate after rechecking session and repository identity.
    /// Once admitted, completion retains authority if its waiter is cancelled.
    pub async fn apply_prepared(&self, prepared: PreparedProfileChange) -> ProfileApplyOutcome {
        let last_known_good = prepared.source_candidate.last_known_good();
        if prepared.requires_service() {
            return ProfileApplyOutcome::Rejected {
                last_known_good,
                cause: Box::new(crate::ServiceManagerError::ConsentRequired).into(),
            };
        }
        let current = || {
            prepared.controlled_root == self.controlled.root()
                && prepared.source_candidate.store().root() == self.store.root()
                && std::sync::Arc::ptr_eq(
                    &prepared.session.capture_publication_gate(),
                    &self.session.capture_publication_gate(),
                )
                && (!prepared.source_candidate.changes_runtime()
                    || prepared.expected_runtime == self.session.runtime_revision())
        };
        if !current() {
            return ProfileApplyOutcome::Rejected {
                last_known_good,
                cause: MihomoError::StaleBinding.into(),
            };
        }
        let mut scopes = self.session.write_scopes();
        scopes.extend([
            self.store.root().to_path_buf(),
            self.controlled.root().to_path_buf(),
        ]);
        let lease = match self.store.write_access.acquire_paths_async(scopes).await {
            Ok(lease) => lease,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: ProfileApplicationError::Task(error),
                };
            }
        };
        if !current() {
            return ProfileApplyOutcome::Rejected {
                last_known_good,
                cause: MihomoError::StaleBinding.into(),
            };
        }
        let mut source_candidate = prepared.source_candidate;
        let store = self.store.with_write_lease(&lease);
        let source_candidate = match run_store(move || {
            source_candidate.materialize_for_trial(store)?;
            Ok(source_candidate)
        })
        .await
        {
            Ok(source_candidate) => source_candidate,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        let application = Self {
            store: self.store.with_write_lease(&lease),
            controlled: self.controlled.with_write_lease(&lease),
            session: self.session.clone(),
            #[cfg(test)]
            commit_gate: self.commit_gate.clone(),
        };
        application
            .apply_source_candidate_expected(
                source_candidate,
                prepared.overrides,
                Some(prepared.expected_runtime),
                prepared.runtime,
            )
            .await
    }

    #[cfg(test)]
    async fn apply_source_candidate(
        &self,
        source_candidate: ProfileSourceCandidate,
        overrides: Vec<PathBuf>,
    ) -> ProfileApplyOutcome {
        self.apply_source_candidate_expected(source_candidate, overrides, None, None)
            .await
    }

    async fn apply_source_candidate_expected(
        &self,
        source_candidate: ProfileSourceCandidate,
        overrides: Vec<PathBuf>,
        expected: Option<crate::core_session::RuntimeConfigurationRevision>,
        prepared_runtime: Option<crate::core_session::service_tun::PreparedConfig>,
    ) -> ProfileApplyOutcome {
        let last_known_good = source_candidate.last_known_good();
        let source_metadata = ProfileSourceMetadata::from(source_candidate.record());
        let recovery = ProfileRecovery {
            attempted: source_metadata.clone(),
            last_known_good: last_known_good.clone(),
        };
        let changes_runtime = source_candidate.changes_runtime();
        let runtime = match self
            .session
            .stage_profile_application_expected(
                &self.controlled,
                prepared_runtime.map_or_else(
                    || {
                        crate::profile::ProfileRuntimeSource::Frozen(
                            source_candidate.payload().clone(),
                        )
                    },
                    |prepared| prepared.into_profile_source(),
                ),
                source_candidate.previous_path(),
                overrides,
                changes_runtime,
                expected.filter(|_| changes_runtime),
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(CoreProfileStageError::RuntimeUnknown {
                cause,
                runtime_version,
            }) => {
                return ProfileApplyOutcome::RuntimeUnknown {
                    recovery,
                    cause: cause.into(),
                    runtime_version,
                };
            }
            Err(CoreProfileStageError::Rejected(cause)) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: cause.into(),
                };
            }
        };

        let failure_recovery = recovery.clone();
        #[cfg(test)]
        let commit_gate = self.commit_gate.clone();
        // Admission already holds the session transition and all write authority.
        // Hand that single transaction to its completion owner before starting persistence.
        let completion = tokio::spawn(async move {
            let commit = run_store(move || {
                #[cfg(test)]
                if let Some(gate) = commit_gate {
                    gate.wait();
                }
                source_candidate.commit()
            })
            .await;
            match commit {
                Ok(committed) => match runtime.commit(committed.path.clone()).await {
                    Ok(Some(applied)) => ProfileApplyOutcome::Applied {
                        profile: committed.record,
                        path: committed.path,
                        source_version: source_metadata,
                        runtime_version: applied.generation,
                        kind: applied.kind,
                    },
                    Ok(None) => ProfileApplyOutcome::Stored {
                        profile: committed.record,
                        path: committed.path,
                        source_version: source_metadata,
                    },
                    Err((cause, runtime_version)) => {
                        ProfileApplyOutcome::CommittedButRuntimeUnknown {
                            profile: committed.record,
                            path: committed.path,
                            source_version: source_metadata,
                            cause: cause.into(),
                            runtime_version,
                        }
                    }
                },
                Err(cause) => match runtime.rollback().await {
                    CoreProfileRollbackOutcome::Runtime {
                        result: Ok(()),
                        runtime_version,
                    } => ProfileApplyOutcome::RolledBack {
                        last_known_good,
                        cause,
                        runtime_version,
                    },
                    CoreProfileRollbackOutcome::Validated => ProfileApplyOutcome::Rejected {
                        last_known_good,
                        cause,
                    },
                    CoreProfileRollbackOutcome::Runtime {
                        result: Err(rollback),
                        runtime_version,
                    } => ProfileApplyOutcome::PersistedButRuntimeUnknown {
                        recovery,
                        cause,
                        rollback: rollback.into(),
                        runtime_version,
                    },
                },
            }
        });
        completion
            .await
            .unwrap_or_else(|error| ProfileApplyOutcome::RuntimeUnknown {
                recovery: failure_recovery,
                cause: ProfileApplicationError::Task(error.to_string()),
                runtime_version: self.session.invalidate_runtime_observations(),
            })
    }
}

async fn run_store<T, F>(operation: F) -> Result<T, ProfileApplicationError>
where
    T: Send + 'static,
    F: FnOnce() -> ProfileStoreResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| ProfileApplicationError::Task(error.to_string()))?
        .map_err(ProfileApplicationError::Store)
}
