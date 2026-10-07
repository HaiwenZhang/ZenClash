//! Transaction owner for applying managed profiles to the runtime core.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

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

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One managed source revision used to correlate persistent and runtime state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileVersion {
    /// Stable managed-profile identifier.
    pub profile_id: String,
    /// Last source update timestamp recorded by the profile store.
    pub updated_at: u64,
    /// Persisted source payload size.
    pub size_bytes: u64,
}

impl From<&ProfileRecord> for ProfileVersion {
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
    /// Source revision whose application was attempted.
    pub attempted: ProfileVersion,
    /// Source revision that was active before the attempt, when known.
    pub last_known_good: Option<ProfileVersion>,
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
    #[error("配置事务后台任务异常结束：{0}")]
    Task(String),
}

/// Rejection while preparing a source candidate, before runtime state changes.
#[derive(Debug, Error)]
#[error("{cause}")]
pub struct ProfilePreparationError {
    /// Last active managed source observed during preparation.
    pub last_known_good: Option<ProfileVersion>,
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
        /// Persistent source revision that was applied.
        source_version: ProfileVersion,
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
        /// Persistent source revision that was committed.
        source_version: ProfileVersion,
    },
    /// The change was rejected before runtime state changed.
    Rejected {
        /// Last active source revision observed before the attempt.
        last_known_good: Option<ProfileVersion>,
        /// Typed rejection cause.
        cause: ProfileApplicationError,
    },
    /// Persistence failed after runtime acceptance, and the prior runtime was restored.
    RolledBack {
        /// Restored source revision, when one was active previously.
        last_known_good: Option<ProfileVersion>,
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
        /// Source revision saved before the finalization failure.
        source_version: ProfileVersion,
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
    staged: StagedProfile,
    overrides: Vec<PathBuf>,
    binding: u64,
    generation: u64,
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

struct StagedProfile {
    store: ProfileStore,
    record: ProfileRecord,
    candidate_path: PathBuf,
    payload: std::sync::Arc<str>,
    expected_catalog: ProfileCatalog,
    disposition: StagedProfileDisposition,
    _write_lease: Option<crate::data_coordinator::DataWriteLease>,
    materialized: bool,
}

enum StagedProfileDisposition {
    ExistingActivation {
        source_path: PathBuf,
        expected_payload: Vec<u8>,
    },
    New,
    Update {
        source_path: PathBuf,
        expected_payload: Vec<u8>,
    },
}

struct CommittedProfile {
    record: ProfileRecord,
    path: PathBuf,
}

/// One-shot catalog participant in the admitted service transaction.
pub(crate) struct ServiceProfileCommit {
    root: PathBuf,
    staged: parking_lot::Mutex<Option<StagedProfile>>,
    committed: parking_lot::Mutex<Option<CommittedProfile>>,
    failure: parking_lot::Mutex<Option<ProfileApplicationError>>,
    runtime_attempted: std::sync::atomic::AtomicBool,
    runtime_generation: parking_lot::Mutex<Option<u64>>,
}

impl ServiceProfileCommit {
    fn new(staged: StagedProfile) -> Self {
        Self {
            root: staged.store.root().to_path_buf(),
            staged: parking_lot::Mutex::new(Some(staged)),
            committed: parking_lot::Mutex::new(None),
            failure: parking_lot::Mutex::new(None),
            runtime_attempted: std::sync::atomic::AtomicBool::new(false),
            runtime_generation: parking_lot::Mutex::new(None),
        }
    }

    pub(crate) fn mark_runtime_attempted(&self) {
        self.runtime_attempted.store(true, Ordering::Release);
    }

    pub(crate) fn record_runtime_generation(&self, generation: u64) {
        if self.runtime_attempted.load(Ordering::Acquire) {
            *self.runtime_generation.lock() = Some(generation);
        }
    }

    fn unsaved_outcome(
        &self,
        session: &CoreSession,
        recovery: ProfileRecovery,
        cause: ProfileApplicationError,
        restored: bool,
    ) -> ProfileApplyOutcome {
        if !self.runtime_attempted.load(Ordering::Acquire) {
            return ProfileApplyOutcome::Rejected {
                last_known_good: recovery.last_known_good,
                cause,
            };
        }
        let generation = self
            .runtime_generation
            .lock()
            .unwrap_or_else(|| session.mark_runtime_unknown());
        if restored {
            ProfileApplyOutcome::RolledBack {
                last_known_good: recovery.last_known_good,
                cause,
                runtime_version: generation,
            }
        } else {
            ProfileApplyOutcome::RuntimeUnknown {
                recovery,
                cause,
                runtime_version: generation,
            }
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) async fn validate(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        self.run(lease, false).await
    }

    pub(crate) async fn commit(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        self.run(lease, true).await
    }

    async fn run(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
        commit: bool,
    ) -> Result<(), CoreSessionError> {
        if !lease.covers(&self.root) {
            return Err(MihomoError::StaleBinding.into());
        }
        let participant = self.clone();
        let lease = crate::data_coordinator::DataWriteAccess::new(&self.root)
            .authorized(lease)
            .borrowed_authority(std::slice::from_ref(&self.root))
            .map_err(crate::ControlledConfigError::Transaction)?
            .ok_or(MihomoError::StaleBinding)?;
        let result = run_store(move || {
            let mut pending = participant.staged.lock();
            let staged = pending.as_mut().ok_or_else(|| {
                ProfileStoreError::Transaction(zenclash_i18n::text("core_page.service.unknown"))
            })?;
            staged.validate_source_current()?;
            if commit {
                // Materialize only after the runtime trial, under the shared transaction lease.
                staged.store = staged.store.with_write_lease(&lease);
                staged._write_lease = Some(staged.store.write_access.acquire());
                staged.candidate_path =
                    write_staging_payload(staged.store.root(), staged.payload.as_bytes())?;
                staged.materialized = true;
                let staged = pending.take().ok_or_else(|| {
                    ProfileStoreError::Transaction(zenclash_i18n::text("core_page.service.unknown"))
                })?;
                *participant.committed.lock() = Some(staged.commit()?);
            }
            Ok(())
        })
        .await;
        result.map_err(|error| {
            let message = error.to_string();
            *self.failure.lock() = Some(error);
            crate::ControlledConfigError::Transaction(message).into()
        })
    }
}

impl StagedProfile {
    fn existing(store: ProfileStore, id: &str) -> ProfileStoreResult<Self> {
        let _write_lease = store.write_access.acquire();
        let _transaction = store.transaction.lock();
        let catalog = store.load_unlocked()?;
        let record = catalog
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
        let source_path = store.profile_path(&record);
        let payload = read_profile_bytes(&source_path)?;
        let frozen: std::sync::Arc<str> = String::from_utf8(payload.clone())
            .map_err(|error| ProfileStoreError::InvalidYaml(error.to_string()))?
            .into();
        let candidate_path = write_staging_payload(store.root(), &payload)?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            candidate_path,
            payload: frozen,
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            materialized: true,
            disposition: StagedProfileDisposition::ExistingActivation {
                source_path,
                expected_payload: payload,
            },
        })
    }

    fn local(store: ProfileStore, source: &Path) -> ProfileStoreResult<Self> {
        let payload = String::from_utf8(read_profile_bytes(source)?).map_err(|error| {
            ProfileStoreError::InvalidYaml(format!("本地配置内容不是 UTF-8：{error}"))
        })?;
        validate_clash_yaml(&payload)?;
        let name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("本地配置")
            .to_owned();
        Self::new(
            store,
            name,
            ProfileSource::Local {
                original_path: source.display().to_string(),
            },
            payload,
            SubscriptionMetadata::default(),
        )
    }

    fn new(
        store: ProfileStore,
        name: String,
        source: ProfileSource,
        payload: String,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&payload)?;
        let _write_lease = store.write_access.acquire();
        let _transaction = store.transaction.lock();
        let catalog = store.load_unlocked()?;
        let record =
            ProfileStore::new_profile_record(&catalog, name, source, payload.len(), subscription);
        let candidate_path = write_staging_payload(store.root(), payload.as_bytes())?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            candidate_path,
            payload: payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            materialized: true,
            disposition: StagedProfileDisposition::New,
        })
    }

    fn remote_update(
        store: ProfileStore,
        expected_record: &ProfileRecord,
        payload: String,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&payload)?;
        let _write_lease = store.write_access.acquire();
        let _transaction = store.transaction.lock();
        let catalog = store.load_unlocked()?;
        let index = catalog
            .profiles
            .iter()
            .position(|profile| profile.id == expected_record.id)
            .ok_or_else(|| ProfileStoreError::NotFound(expected_record.id.clone()))?;
        if &catalog.profiles[index] != expected_record || !expected_record.is_remote() {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {} 在订阅下载期间已改变，拒绝覆盖较新版本",
                expected_record.id
            )));
        }
        let source_path = store.profile_path(expected_record);
        let expected_payload = read_profile_bytes(&source_path)?;
        let mut record = expected_record.clone();
        record.updated_at = super::unix_timestamp();
        record.size_bytes = payload.len() as u64;
        record.subscription.merge_from(subscription);
        let accepts_suggested_interval = matches!(
            &record.source,
            ProfileSource::Remote { options, .. } if !options.fixed_update_interval
        );
        if let Some(interval) = accepts_suggested_interval
            .then_some(record.subscription.suggested_update_interval_minutes)
            .flatten()
        {
            record.update_interval_minutes = interval;
        }
        let candidate_path = write_staging_payload(store.root(), payload.as_bytes())?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            candidate_path,
            payload: payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            materialized: true,
            disposition: StagedProfileDisposition::Update {
                source_path,
                expected_payload,
            },
        })
    }

    fn edit(
        store: ProfileStore,
        id: &str,
        expected_payload: &str,
        new_payload: String,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&new_payload)?;
        let _write_lease = store.write_access.acquire();
        let _transaction = store.transaction.lock();
        let catalog = store.load_unlocked()?;
        let record = catalog
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
        let source_path = store.profile_path(&record);
        let persisted_payload = read_profile_bytes(&source_path)?;
        if persisted_payload != expected_payload.as_bytes() {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {id} 在编辑期间已改变，请重新打开后再保存"
            )));
        }
        let mut record = record;
        record.updated_at = super::unix_timestamp();
        record.size_bytes = new_payload.len() as u64;
        let candidate_path = write_staging_payload(store.root(), new_payload.as_bytes())?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            candidate_path,
            payload: new_payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            materialized: true,
            disposition: StagedProfileDisposition::Update {
                source_path,
                expected_payload: persisted_payload,
            },
        })
    }

    fn last_known_good(&self) -> Option<ProfileVersion> {
        self.expected_catalog
            .active_profile()
            .map(ProfileVersion::from)
    }

    fn previous_path(&self) -> Option<PathBuf> {
        self.expected_catalog
            .active_profile()
            .map(|profile| self.store.profile_path(profile))
    }

    fn record(&self) -> &ProfileRecord {
        &self.record
    }

    fn changes_runtime(&self) -> bool {
        match &self.disposition {
            StagedProfileDisposition::ExistingActivation { .. } | StagedProfileDisposition::New => {
                true
            }
            StagedProfileDisposition::Update { .. } => {
                self.expected_catalog.active.as_deref() == Some(self.record.id.as_str())
            }
        }
    }

    fn validate_source_current(&self) -> ProfileStoreResult<()> {
        let _transaction = self.store.transaction.lock();
        self.validate_source_against_catalog(&self.store.load_unlocked()?)
    }

    fn validate_source_against_catalog(&self, catalog: &ProfileCatalog) -> ProfileStoreResult<()> {
        if *catalog != self.expected_catalog {
            return Err(ProfileStoreError::Transaction(
                "配置目录在候选验证期间已改变，请刷新后重试".into(),
            ));
        }
        match &self.disposition {
            StagedProfileDisposition::ExistingActivation {
                source_path,
                expected_payload,
            }
            | StagedProfileDisposition::Update {
                source_path,
                expected_payload,
            } => {
                if read_profile_bytes(source_path)? != *expected_payload {
                    return Err(ProfileStoreError::Transaction(format!(
                        "配置 {} 在候选验证期间已改变，请刷新后重试",
                        self.record.id
                    )));
                }
            }
            StagedProfileDisposition::New => {}
        }
        Ok(())
    }

    fn commit(self) -> ProfileStoreResult<CommittedProfile> {
        let _write_lease = self.store.write_access.acquire();
        let _transaction = self.store.transaction.lock();
        let mut catalog = self.store.load_unlocked()?;
        if read_profile_bytes(&self.candidate_path)?.as_slice() != self.payload.as_bytes() {
            return Err(ProfileStoreError::Transaction(zenclash_i18n::text(
                "profiles.errors.candidate_changed",
            )));
        }
        self.validate_source_against_catalog(&catalog)?;

        match &self.disposition {
            StagedProfileDisposition::ExistingActivation {
                source_path,
                expected_payload: _,
            } => {
                catalog.active = Some(self.record.id.clone());
                self.store.save_unlocked(&catalog)?;
                Ok(CommittedProfile {
                    record: self.record.clone(),
                    path: source_path.clone(),
                })
            }
            StagedProfileDisposition::New => {
                let payload = self.payload.as_bytes();
                let path = self.store.profile_path(&self.record);
                atomic_write(&path, payload)?;
                catalog.profiles.push(self.record.clone());
                catalog.active = Some(self.record.id.clone());
                if let Err(error) = self.store.save_unlocked(&catalog) {
                    return match fs::remove_file(&path) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                            "保存配置索引失败：{error}；清理未入库配置失败：{rollback}"
                        ))),
                    };
                }
                Ok(CommittedProfile {
                    record: self.record.clone(),
                    path,
                })
            }
            StagedProfileDisposition::Update {
                source_path,
                expected_payload,
            } => {
                let index = catalog
                    .profiles
                    .iter()
                    .position(|profile| profile.id == self.record.id)
                    .ok_or_else(|| ProfileStoreError::NotFound(self.record.id.clone()))?;
                atomic_write(source_path, self.payload.as_bytes())?;
                catalog.profiles[index] = self.record.clone();
                self.store.save_catalog_or_restore_payload_unlocked(
                    &catalog,
                    source_path,
                    expected_payload,
                    "保存配置索引失败",
                    "恢复上一版本配置失败",
                )?;
                Ok(CommittedProfile {
                    record: self.record.clone(),
                    path: source_path.clone(),
                })
            }
        }
    }
}

impl Drop for StagedProfile {
    fn drop(&mut self) {
        if self.materialized {
            let _ = fs::remove_file(&self.candidate_path);
        }
    }
}

fn write_staging_payload(root: &Path, payload: &[u8]) -> ProfileStoreResult<PathBuf> {
    if payload.len() > MAX_PROFILE_BYTES {
        return Err(ProfileStoreError::InvalidYaml(format!(
            "配置文件超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )));
    }
    let staging = root.join("staging");
    fs::create_dir_all(&staging)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = staging.join(format!(
        "candidate-{}-{timestamp}-{sequence}.yaml",
        std::process::id()
    ));
    atomic_write(&path, payload)?;
    Ok(path)
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
        let binding = self.session.runtime_descriptor().binding_generation();
        let generation = self.session.generation();
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
        let (mut staged, overrides) = application.stage_change(change).await?;
        let last_known_good = staged.last_known_good();
        let runtime = if staged.changes_runtime()
            && self.session.kind() == crate::CoreKind::Mihomo
            && self.session.runtime_descriptor().backend() == crate::CoreRuntimeBackend::Local
        {
            Some(
                self.session
                    .prepare_service_profile_config(
                        &application.controlled,
                        (binding, generation),
                        staged.store.profile_path(&staged.record),
                        staged.payload.clone(),
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
        let staged = run_store(move || {
            fs::remove_file(&staged.candidate_path)?;
            staged.materialized = false;
            staged.store = ordinary_store;
            staged._write_lease = None;
            Ok(staged)
        })
        .await
        .map_err(|cause| ProfilePreparationError {
            last_known_good,
            cause,
        })?;
        Ok(PreparedProfileChange {
            staged,
            overrides,
            binding,
            generation,
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

    /// Applies a managed change, authorizing Service ownership for an effective TUN candidate.
    /// Returns capture facts separately from the directory receipt. Once admitted,
    /// completion retains both receipts even when its waiter stops waiting.
    pub async fn apply_with_service(
        &self,
        manager: &crate::ServiceManager,
        change: ProfileChange,
    ) -> (ProfileApplyOutcome, Option<crate::ServiceTunOutcome>) {
        let prepared = match self.prepare_change(change).await {
            Ok(prepared) => prepared,
            Err(error) => return (error.into(), None),
        };
        if !prepared.requires_service() {
            return (self.apply_prepared(prepared).await, None);
        }
        let last_known_good = prepared.staged.last_known_good();
        let source_version = ProfileVersion::from(prepared.staged.record());
        let recovery = ProfileRecovery {
            attempted: source_version.clone(),
            last_known_good: last_known_good.clone(),
        };
        if !manager.owns_profile_application(&self.session, &self.controlled) {
            return (
                ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: MihomoError::StaleBinding.into(),
                },
                None,
            );
        }
        let Some(crate::core_session::service_tun::PreparedConfig::Service(runtime)) =
            prepared.runtime
        else {
            return (
                ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: MihomoError::StaleBinding.into(),
                },
                None,
            );
        };
        let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.staged));
        let mut runtime = (*runtime).clone();
        runtime.profile_commit = Some(participant.clone());
        let failure_recovery = recovery.clone();
        let application = self.clone();
        let manager = manager.clone();
        let completion = tokio::spawn(async move {
            let result = manager
                .apply_service_profile(std::sync::Arc::new(runtime))
                .await;
            let committed = participant.committed.lock().take();
            let failure = participant.failure.lock().take();
            let (service, mut manager_failure) = match result {
                Ok(service) => (Some(service), None),
                Err(error) => (
                    None,
                    Some(ProfileApplicationError::Service(Box::new(error))),
                ),
            };
            if let Some(committed) = committed {
                let core = service.as_ref().and_then(crate::ServiceTunOutcome::core);
                let pending = service
                    .as_ref()
                    .is_none_or(|service| service.commit_pending());
                let outcome = if let Some(core) = core.filter(|_| !pending) {
                    ProfileApplyOutcome::Applied {
                        profile: committed.record,
                        path: committed.path,
                        source_version,
                        runtime_version: core.generation,
                        kind: core.kind,
                    }
                } else {
                    ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        profile: committed.record,
                        path: committed.path,
                        source_version,
                        cause: manager_failure.take().unwrap_or_else(|| {
                            ProfileApplicationError::Controller(MihomoError::Process(
                                zenclash_i18n::text("core_page.service.pending"),
                            ))
                        }),
                        runtime_version: core.map_or_else(
                            || {
                                participant
                                    .runtime_generation
                                    .lock()
                                    .unwrap_or_else(|| application.session.mark_runtime_unknown())
                            },
                            |core| core.generation,
                        ),
                    }
                };
                return (outcome, service);
            }
            let restored = service.as_ref().is_some_and(|service| {
                matches!(service.capture(), crate::CaptureOutcome::RolledBack { .. })
            });
            let cause = failure.or(manager_failure).unwrap_or_else(|| {
                ProfileApplicationError::Controller(MihomoError::Process(
                    match service.as_ref().map(crate::ServiceTunOutcome::capture) {
                        Some(
                            crate::CaptureOutcome::RolledBack { failure, .. }
                            | crate::CaptureOutcome::ReconcileNeeded { failure, .. },
                        ) => failure.clone(),
                        _ => zenclash_i18n::text("core_page.service.unknown"),
                    },
                ))
            });
            let outcome =
                participant.unsaved_outcome(&application.session, recovery, cause, restored);
            (outcome, service)
        });
        completion.await.unwrap_or_else(|error| {
            (
                ProfileApplyOutcome::RuntimeUnknown {
                    recovery: failure_recovery,
                    cause: ProfileApplicationError::Task(error.to_string()),
                    runtime_version: self.session.mark_runtime_unknown(),
                },
                None,
            )
        })
    }

    /// Consumes one held candidate after rechecking session and repository identity.
    /// Once admitted, completion retains authority if its waiter is cancelled.
    pub async fn apply_prepared(&self, prepared: PreparedProfileChange) -> ProfileApplyOutcome {
        let last_known_good = prepared.staged.last_known_good();
        if prepared.requires_service() {
            return ProfileApplyOutcome::Rejected {
                last_known_good,
                cause: Box::new(crate::ServiceManagerError::ConsentRequired).into(),
            };
        }
        let current = || {
            prepared.controlled_root == self.controlled.root()
                && prepared.staged.store.root() == self.store.root()
                && std::sync::Arc::ptr_eq(
                    &prepared.session.capture_publication_gate(),
                    &self.session.capture_publication_gate(),
                )
                && (!prepared.staged.changes_runtime()
                    || (prepared.binding == self.session.runtime_descriptor().binding_generation()
                        && prepared.generation == self.session.generation()))
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
        let mut staged = prepared.staged;
        let store = self.store.with_write_lease(&lease);
        let staged = match run_store(move || {
            staged.store = store;
            staged._write_lease = Some(staged.store.write_access.acquire());
            staged.validate_source_current()?;
            staged.candidate_path =
                write_staging_payload(staged.store.root(), staged.payload.as_bytes())?;
            staged.materialized = true;
            Ok(staged)
        })
        .await
        {
            Ok(staged) => staged,
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
            .apply_staged_expected(
                staged,
                prepared.overrides,
                Some((prepared.binding, prepared.generation)),
                prepared.runtime,
            )
            .await
    }

    async fn stage_change(
        &self,
        change: ProfileChange,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        match change {
            ProfileChange::ImportLocal { source, overrides } => {
                self.import_local(source, overrides).await
            }
            ProfileChange::AddRemote {
                name,
                url,
                user_agent,
                options,
                overrides,
            } => {
                self.add_remote(name, url, user_agent, options, overrides)
                    .await
            }
            ProfileChange::UpdateRemote { id, overrides } => {
                self.update_remote(id, overrides).await
            }
            ProfileChange::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            } => {
                self.edit_yaml(id, expected_payload, new_payload, overrides)
                    .await
            }
            ProfileChange::ActivateExisting { id, overrides } => {
                self.activate_existing(id, overrides).await
            }
        }
    }

    async fn import_local(
        &self,
        source: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || StagedProfile::local(store, &source)).await {
            Ok(staged) => staged,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((staged, overrides))
    }

    async fn update_remote(
        &self,
        id: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        let store = self.store.clone();
        let lookup_id = id.clone();
        let (expected_record, last_known_good) = match run_store(move || {
            let catalog = store.load()?;
            let record = catalog
                .profiles
                .iter()
                .find(|profile| profile.id == lookup_id)
                .cloned()
                .ok_or_else(|| ProfileStoreError::NotFound(lookup_id))?;
            if !record.is_remote() {
                return Err(ProfileStoreError::NotFound(format!(
                    "{} 不是在线订阅",
                    record.id
                )));
            }
            let last_known_good = catalog.active_profile().map(ProfileVersion::from);
            Ok((record, last_known_good))
        })
        .await
        {
            Ok(prepared) => prepared,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &expected_record.source
        else {
            return Err(ProfilePreparationError {
                last_known_good,
                cause: ProfileStoreError::NotFound(format!("{id} 不是在线订阅")).into(),
            });
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        let downloaded = match download_profile(url, user_agent, options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || {
            StagedProfile::remote_update(
                store,
                &expected_record,
                downloaded.payload,
                downloaded.metadata,
            )
        })
        .await
        {
            Ok(staged) => staged,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((staged, overrides))
    }

    async fn edit_yaml(
        &self,
        id: String,
        expected_payload: String,
        new_payload: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || {
            StagedProfile::edit(store, &id, &expected_payload, new_payload)
        })
        .await
        {
            Ok(staged) => staged,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((staged, overrides))
    }

    async fn add_remote(
        &self,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
        overrides: Vec<PathBuf>,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let name = match if name.trim().is_empty() {
            Ok(String::new())
        } else {
            normalized_profile_name(&name)
        } {
            Ok(name) => name,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let url = match normalized_remote_url(&url) {
            Ok(url) => url,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let user_agent = match normalized_user_agent(&user_agent) {
            Ok(user_agent) => user_agent,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        let downloaded = match download_profile(&url, &user_agent, &options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let name = if name.is_empty() {
            downloaded.suggested_name.clone()
        } else {
            name
        };
        let source = ProfileSource::Remote {
            url,
            user_agent,
            options,
        };
        let store = self.store.clone();
        let staged = match run_store(move || {
            StagedProfile::new(store, name, source, downloaded.payload, downloaded.metadata)
        })
        .await
        {
            Ok(staged) => staged,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((staged, overrides))
    }

    async fn activate_existing(
        &self,
        id: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(StagedProfile, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || StagedProfile::existing(store, &id)).await {
            Ok(staged) => staged,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((staged, overrides))
    }

    #[cfg(test)]
    async fn apply_staged(
        &self,
        staged: StagedProfile,
        overrides: Vec<PathBuf>,
    ) -> ProfileApplyOutcome {
        self.apply_staged_expected(staged, overrides, None, None)
            .await
    }

    async fn apply_staged_expected(
        &self,
        staged: StagedProfile,
        overrides: Vec<PathBuf>,
        expected: Option<(u64, u64)>,
        prepared_runtime: Option<crate::core_session::service_tun::PreparedConfig>,
    ) -> ProfileApplyOutcome {
        let last_known_good = staged.last_known_good();
        let source_version = ProfileVersion::from(staged.record());
        let recovery = ProfileRecovery {
            attempted: source_version.clone(),
            last_known_good: last_known_good.clone(),
        };
        let changes_runtime = staged.changes_runtime();
        let runtime = match self
            .session
            .stage_profile_application_expected(
                &self.controlled,
                prepared_runtime.map_or_else(
                    || crate::profile::ProfileRuntimeSource::Frozen(staged.payload.clone()),
                    |prepared| prepared.into_profile_source(),
                ),
                staged.previous_path(),
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
                staged.commit()
            })
            .await;
            match commit {
                Ok(committed) => match runtime.commit(committed.path.clone()).await {
                    Ok(Some(applied)) => ProfileApplyOutcome::Applied {
                        profile: committed.record,
                        path: committed.path,
                        source_version,
                        runtime_version: applied.generation,
                        kind: applied.kind,
                    },
                    Ok(None) => ProfileApplyOutcome::Stored {
                        profile: committed.record,
                        path: committed.path,
                        source_version,
                    },
                    Err((cause, runtime_version)) => {
                        ProfileApplyOutcome::CommittedButRuntimeUnknown {
                            profile: committed.record,
                            path: committed.path,
                            source_version,
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
                runtime_version: self.session.mark_runtime_unknown(),
            })
    }

    async fn last_known_good(&self) -> Result<Option<ProfileVersion>, ProfileApplicationError> {
        let store = self.store.clone();
        run_store(move || store.load())
            .await
            .map(|catalog| catalog.active_profile().map(ProfileVersion::from))
    }

    async fn subscription_proxy_port(
        &self,
        route: RemoteProfileRoute,
    ) -> Result<Option<u16>, ProfileApplicationError> {
        if route == RemoteProfileRoute::Direct {
            return Ok(None);
        }
        let config = match self.session.client().runtime_config().await {
            Ok(config) => config,
            Err(_) if route == RemoteProfileRoute::DirectWithMihomoFallback => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let port = if config.mixed_port != 0 {
            config.mixed_port
        } else {
            config.port
        };
        if port != 0 {
            Ok(Some(port))
        } else if route == RemoteProfileRoute::DirectWithMihomoFallback {
            Ok(None)
        } else {
            Err(MihomoError::InvalidInput(
                "当前内核没有可供订阅下载使用的 HTTP 或 Mixed 端口".into(),
            )
            .into())
        }
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{CoreKind, MihomoClient, MihomoEndpoint};

    use super::*;

    #[tokio::test]
    async fn service_directory_stale_rejection_does_not_claim_a_newer_runtime_failure() {
        let fixture = Fixture::new("service-directory-stale-outcome");
        let application = fixture.application("127.0.0.1:9".parse().unwrap());
        let prepared = application
            .prepare_change(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        let recovery = ProfileRecovery {
            attempted: ProfileVersion::from(prepared.staged.record()),
            last_known_good: prepared.staged.last_known_good(),
        };
        let participant = ServiceProfileCommit::new(prepared.staged);
        let newer = application.session.mark_runtime_unknown();
        let result = participant.unsaved_outcome(
            &application.session,
            recovery,
            Box::new(crate::ServiceManagerError::Stale).into(),
            false,
        );
        assert!(matches!(
            result,
            ProfileApplyOutcome::Rejected {
                cause: ProfileApplicationError::Service(_),
                ..
            }
        ));
        assert_eq!(application.session.generation(), newer);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn service_directory_failure_keeps_its_own_completed_runtime_version() {
        let fixture = Fixture::new("service-directory-failure-version");
        let application = fixture.application("127.0.0.1:9".parse().unwrap());
        let prepared = application
            .prepare_change(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        let recovery = ProfileRecovery {
            attempted: ProfileVersion::from(prepared.staged.record()),
            last_known_good: prepared.staged.last_known_good(),
        };
        let participant = ServiceProfileCommit::new(prepared.staged);
        participant.mark_runtime_attempted();
        let completed = application.session.mark_runtime_unknown();
        participant.record_runtime_generation(completed);
        let newer = application.session.mark_runtime_unknown();
        let result = participant.unsaved_outcome(
            &application.session,
            recovery,
            ProfileApplicationError::Task("trial failed".into()),
            false,
        );
        assert!(
            matches!(result, ProfileApplyOutcome::RuntimeUnknown { runtime_version, .. } if runtime_version == completed)
        );
        assert_eq!(application.session.generation(), newer);
    }

    #[tokio::test]
    async fn service_catalog_participant_defers_commit_and_keeps_held_source() {
        let fixture = Fixture::new("service-catalog-held");
        let application = fixture.application("127.0.0.1:9".parse().unwrap());
        let source = fixture.write_source("new.yaml", "HELD");
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source: source.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        let record = prepared.staged.record.clone();
        let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.staged));
        fs::remove_file(&source).unwrap();
        let lease = fixture
            .controlled
            .acquire_write_lease_for_paths(vec![fixture.store.root().to_path_buf()])
            .await
            .unwrap();
        participant.validate(&lease).await.unwrap();
        assert!(!fixture.store.profile_path(&record).exists());
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        participant.commit(&lease).await.unwrap();
        assert_eq!(
            fs::read_to_string(fixture.store.profile_path(&record)).unwrap(),
            profile_payload("HELD")
        );
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(record.id.as_str())
        );
        assert!(participant.committed.lock().is_some());
        assert!(!source.exists());
        assert!(
            participant.commit(&lease).await.is_err(),
            "the receipt cannot be committed twice"
        );
    }

    #[tokio::test]
    async fn service_catalog_conflict_restores_staged_cache_without_rewriting_controlled_layer() {
        let fixture = Fixture::new("service-catalog-conflict");
        let application = fixture.application("127.0.0.1:9".parse().unwrap());
        let source = fixture.write_source("new.yaml", "HELD");
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source,
                overrides: vec![],
            })
            .await
            .unwrap();
        let destination = fixture.store.profile_path(&prepared.staged.record);
        let payload = prepared.staged.payload.clone();
        let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.staged));
        let lease = fixture
            .controlled
            .acquire_write_lease_for_paths(vec![fixture.store.root().to_path_buf()])
            .await
            .unwrap();
        let controlled = fixture.controlled.with_write_lease(&lease);
        fs::create_dir_all(controlled.root()).unwrap();
        fs::write(controlled.runtime_path(), "mode: direct\n").unwrap();
        let update = controlled
            .prepare_service_config_update(
                crate::profile::ProfileRuntimeSource::Frozen(payload),
                None,
                vec![],
                Some("mode: direct\n".into()),
            )
            .await
            .unwrap();
        participant.validate(&lease).await.unwrap();
        let persistence = controlled.stage_service_tun_update(update).await.unwrap();
        persistence.validate_patch().await.unwrap();
        fixture.store.activate(&fixture.candidate.id).unwrap();
        assert!(participant.commit(&lease).await.is_err());
        persistence.rollback().await.unwrap();
        assert_eq!(
            fs::read_to_string(controlled.runtime_path()).unwrap(),
            "mode: direct\n"
        );
        assert!(!controlled.root().join("override.yaml").exists());
        assert!(!destination.exists());
        assert!(participant.committed.lock().is_none());
        assert!(matches!(
            participant.failure.lock().as_ref(),
            Some(ProfileApplicationError::Store(_))
        ));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.candidate.id.as_str())
        );
    }

    #[tokio::test]
    async fn prepared_tun_directory_cannot_bypass_service_admission() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-directory-service-admission",
        )
        .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ProfileStore::new(home.join("profiles")).unwrap();
        let controlled = ControlledConfigStore::new(home.join("controlled"));
        fs::create_dir_all(controlled.root()).unwrap();
        fs::write(controlled.runtime_path(), "tun: {enable: false}\n").unwrap();
        let source = home.join("tun-source.yaml");
        fs::write(&source, "tun: {enable: true}\n").unwrap();
        let application =
            ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source,
                overrides: vec![],
            })
            .await
            .unwrap();
        assert!(prepared.requires_service());
        let pid = fixture.process.snapshot().pid;
        let result = application.apply_prepared(prepared).await;
        assert!(matches!(
            result,
            ProfileApplyOutcome::Rejected {
                cause: ProfileApplicationError::Service(_),
                ..
            }
        ));
        assert!(store.load().unwrap().profiles.is_empty());
        assert_eq!(
            fs::read_to_string(controlled.runtime_path()).unwrap(),
            "tun: {enable: false}\n"
        );
        assert_eq!(session.generation(), 0);
        assert_eq!(fixture.process.snapshot().pid, pid);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_the_waiter_does_not_abandon_admitted_profile_persistence() {
        let fixture = Fixture::new("cancel-persistence");
        fixture
            .controlled
            .materialize_with_overrides_for_core(
                fixture.store.profile_path(&fixture.previous),
                &[],
                CoreKind::Mihomo,
            )
            .unwrap();
        let (address, server) =
            response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
        let mut application = fixture.application(address);
        let session = application.session.clone();
        let (gate, waiting, release) = crate::controlled_config::CommitGate::new();
        application.commit_gate = Some(gate);
        let id = fixture.candidate.id.clone();
        let outer = tokio::spawn(async move {
            application
                .apply(ProfileChange::ActivateExisting {
                    id,
                    overrides: vec![],
                })
                .await
        });
        waiting.await.unwrap();
        outer.abort();
        assert!(outer.await.unwrap_err().is_cancelled());
        let shutdown_session = session.clone();
        let shutdown = tokio::spawn(async move { shutdown_session.shutdown().await });
        tokio::task::yield_now().await;
        let shutdown_waited = !shutdown.is_finished();
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if fixture.store.load().unwrap().active.as_deref()
                    == Some(fixture.candidate.id.as_str())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        shutdown.await.unwrap().unwrap();
        server.join().unwrap();
        assert!(
            shutdown_waited,
            "shutdown must wait for the admitted persistence owner"
        );
        assert!(
            std::fs::read_to_string(fixture.controlled.runtime_path())
                .unwrap()
                .contains("MATCH,REJECT")
        );
        // Shutdown waits for admitted persistence, then publishes its own transition.
        assert_eq!(session.generation(), 2);
        assert_eq!(
            session.committed_profile_snapshot().profile_path.as_deref(),
            Some(fixture.store.profile_path(&fixture.candidate).as_path())
        );
    }

    #[tokio::test]
    async fn prepared_local_directory_applies_final_layers_after_override_is_deleted() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-prepared-directory-final-layers",
        )
        .await;
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ProfileStore::new(home.join("profiles")).unwrap();
        let controlled = ControlledConfigStore::new(home.join("controlled"));
        let source = home.join("new-source.yaml");
        fs::write(&source, "tun: {enable: true}\nmode: global\n").unwrap();
        let override_path = home.join("ordered.yaml");
        fs::write(&override_path, "tun: {enable: false}\nmode: direct\n").unwrap();
        let application =
            ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source: source.clone(),
                overrides: vec![override_path.clone()],
            })
            .await
            .unwrap();
        assert!(!prepared.requires_service(), "final override disables TUN");
        fs::remove_file(&source).unwrap();
        fs::remove_file(override_path).unwrap();
        let outcome = application.apply_prepared(prepared).await;
        assert!(
            matches!(
                outcome,
                ProfileApplyOutcome::Applied {
                    runtime_version: 1,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&fs::read(controlled.runtime_path()).unwrap()).unwrap();
        assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(yaml["mode"].as_str(), Some("direct"));
        let catalog = store.load().unwrap();
        assert_eq!(catalog.profiles.len(), 1);
        assert_eq!(
            fs::read_to_string(store.profile_path(&catalog.profiles[0])).unwrap(),
            "tun: {enable: true}\nmode: global\n"
        );
        session.shutdown().await.unwrap();
        assert!(fixture.process.snapshot().pid.is_none());
    }

    #[tokio::test]
    async fn prepared_local_directory_rejects_changed_controlled_layer_without_committing() {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-prepared-directory-controlled-conflict",
        )
        .await;
        let session = CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ProfileStore::new(home.join("profiles")).unwrap();
        let controlled = ControlledConfigStore::new(home.join("controlled"));
        let source = home.join("new-source.yaml");
        fs::write(&source, "tun: {enable: false}\nmode: rule\n").unwrap();
        let application =
            ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source,
                overrides: vec![],
            })
            .await
            .unwrap();
        fs::create_dir_all(controlled.root()).unwrap();
        let changed = "tun: {enable: true}\nmode: global\n";
        fs::write(controlled.root().join("override.yaml"), changed).unwrap();
        let pid = fixture.process.snapshot().pid;
        let outcome = application.apply_prepared(prepared).await;
        assert!(
            matches!(outcome, ProfileApplyOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(session.generation(), 0);
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert!(store.load().unwrap().profiles.is_empty());
        assert!(!controlled.runtime_path().exists());
        assert_eq!(
            fs::read_to_string(controlled.root().join("override.yaml")).unwrap(),
            changed
        );
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn prepared_import_releases_restore_authority_and_applies_held_source() {
        let fixture = Fixture::new("prepared-import-held");
        let source = fixture.write_source("held.yaml", "HELD");
        let (address, server) = response_server(
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        );
        let application = fixture.application(address);
        let prepared = application
            .prepare_change(ProfileChange::ImportLocal {
                source: source.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        assert_eq!(fixture.store.load().unwrap().profiles.len(), 2);
        assert_eq!(
            fs::read_dir(fixture.store.root().join("staging"))
                .unwrap()
                .count(),
            0
        );
        let root = fixture.root.clone();
        let exclusive = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::task::spawn_blocking(move || {
                crate::data_coordinator::DataWriteLease::exclusive([root])
            }),
        )
        .await
        .expect("preparation must release restore admission")
        .unwrap();
        drop(exclusive);
        fs::remove_file(&source).unwrap();
        let outcome = application.apply_prepared(prepared).await;
        let request = server.join().unwrap();
        let ProfileApplyOutcome::Applied {
            path,
            runtime_version,
            ..
        } = outcome
        else {
            panic!("held import failed: {outcome:?}");
        };
        assert_eq!(runtime_version, 1);
        assert!(request.starts_with("PUT /configs?force=true "));
        assert_eq!(fs::read_to_string(path).unwrap(), profile_payload("HELD"));
        assert!(!source.exists());
        assert_eq!(
            fs::read_dir(fixture.store.root().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn prepared_activation_rejects_changed_source_before_runtime_apply() {
        let fixture = Fixture::new("prepared-activation-changed");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let application = fixture.application(listener.local_addr().unwrap());
        let prepared = application
            .prepare_change(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        let path = fixture.store.profile_path(&fixture.candidate);
        fs::write(&path, profile_payload("CHANGED")).unwrap();
        let outcome = application.apply_prepared(prepared).await;
        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(application.session.generation(), 0);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            profile_payload("CHANGED")
        );
    }

    #[tokio::test]
    async fn prepared_activation_cannot_cross_sessions_with_matching_versions() {
        let fixture = Fixture::new("prepared-cross-session");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let application = fixture.application(address);
        let other = fixture.application(address);
        let prepared = application
            .prepare_change(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        assert_eq!(application.session.generation(), other.session.generation());
        let outcome = other.apply_prepared(prepared).await;
        assert!(matches!(
            outcome,
            ProfileApplyOutcome::Rejected {
                cause: ProfileApplicationError::Controller(MihomoError::StaleBinding),
                ..
            }
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn prepared_activation_cannot_replace_a_newer_runtime_transition() {
        let fixture = Fixture::new("prepared-stale-generation");
        let (address, server) = response_server(
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        );
        let application = fixture.application(address);
        let prepared = application
            .prepare_change(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![],
            })
            .await
            .unwrap();
        let accepted = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.previous.id.clone(),
                overrides: vec![],
            })
            .await;
        assert!(matches!(
            accepted,
            ProfileApplyOutcome::Applied {
                runtime_version: 1,
                ..
            }
        ));
        server.join().unwrap();
        let rejected = application.apply_prepared(prepared).await;
        assert!(matches!(
            rejected,
            ProfileApplyOutcome::Rejected {
                cause: ProfileApplicationError::Controller(MihomoError::StaleBinding),
                ..
            }
        ));
        assert_eq!(application.session.generation(), 1);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn staged_profile_tampering_rolls_back_the_held_candidate_without_committing_catalog() {
        let fixture = Fixture::new("held-staged-source");
        fixture
            .controlled
            .materialize(fixture.store.profile_path(&fixture.previous))
            .unwrap();
        let previous = fixture
            .controlled
            .cached_runtime_payload()
            .unwrap()
            .unwrap();
        let payload = format!("tun: {{enable: false}}\n{}", profile_payload("HELD"));
        let staged = StagedProfile::new(
            fixture.store.clone(),
            "Held".into(),
            ProfileSource::Local {
                original_path: fixture.root.join("original.yaml").display().to_string(),
            },
            payload,
            SubscriptionMetadata::default(),
        )
        .unwrap();
        fs::write(
            &staged.candidate_path,
            format!("tun: {{enable: true}}\n{}", profile_payload("CHANGED")),
        )
        .unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut requests = Vec::new();
            while requests.len() < 2 && std::time::Instant::now() < deadline {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                requests.push(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
                reader.get_mut().write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            requests
        });
        let application = fixture.application(address);
        let outcome = application.apply_staged(staged, vec![]).await;
        let requests = server.join().unwrap();
        assert!(matches!(
            outcome,
            ProfileApplyOutcome::RolledBack {
                runtime_version: 1,
                ..
            }
        ));
        let accepted: serde_yaml::Value =
            serde_yaml::from_str(requests[0]["payload"].as_str().unwrap()).unwrap();
        assert_eq!(accepted["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(accepted["rules"][0].as_str(), Some("MATCH,HELD"));
        assert_eq!(requests[1]["payload"].as_str(), Some(previous.as_str()));
        assert_eq!(
            fixture
                .controlled
                .cached_runtime_payload()
                .unwrap()
                .as_deref(),
            Some(previous.as_str())
        );
        let catalog = fixture.store.load().unwrap();
        assert_eq!(
            catalog.active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        assert_eq!(catalog.profiles.len(), 2);
        assert_eq!(
            fs::read_dir(fixture.store.root().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn existing_profile_is_applied_through_one_transaction_interface() {
        let fixture = Fixture::new("applied");
        let (address, server) =
            response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        let request = server.join().unwrap();

        let ProfileApplyOutcome::Applied {
            profile,
            source_version,
            runtime_version,
            kind,
            ..
        } = outcome
        else {
            panic!("expected applied profile outcome");
        };
        assert!(request.starts_with("PUT /configs?force=true "));
        assert_eq!(profile.id, fixture.candidate.id);
        assert_eq!(source_version.profile_id, profile.id);
        assert_eq!(runtime_version, 1);
        assert_eq!(kind, CoreApplyKind::HotReloaded);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(profile.id.as_str())
        );
    }

    #[tokio::test]
    async fn active_profile_is_not_committed_before_runtime_accepts_the_candidate() {
        let fixture = Fixture::new("staged-activation");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let observed_store = fixture.store.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut first_byte = [0_u8; 1];
            stream.read_exact(&mut first_byte).unwrap();
            let active = observed_store.load().unwrap().active;

            let mut reader = BufReader::new(&mut stream);
            let mut content_length = None;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut body = vec![0_u8; content_length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            drop(reader);

            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            active
        });
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        let active_during_runtime_apply = server.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Applied { .. }));
        assert_eq!(active_during_runtime_apply, Some(fixture.previous.id));
    }

    #[tokio::test]
    async fn runtime_is_restored_when_the_staged_source_changes_before_commit() {
        let fixture = Fixture::new("commit-race");
        fixture
            .controlled
            .materialize_with_overrides_for_core(
                fixture.store.profile_path(&fixture.previous),
                &[],
                CoreKind::Mihomo,
            )
            .unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let candidate_path = fixture.store.profile_path(&fixture.candidate);
        let server = thread::spawn(move || {
            let mut observed = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 8_192];
                let bytes = stream.read(&mut request).unwrap();
                observed.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
                if request_index == 0 {
                    fs::write(&candidate_path, profile_payload("RACE")).unwrap();
                }
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            observed
        });
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        let observed_runtime_payloads = server.join().unwrap();

        assert!(matches!(
            outcome,
            ProfileApplyOutcome::RolledBack {
                runtime_version: 1,
                ..
            }
        ));
        assert!(observed_runtime_payloads[0].contains("MATCH,REJECT"));
        assert!(observed_runtime_payloads[1].contains("MATCH,DIRECT"));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn runtime_rejection_leaves_the_previous_active_profile_untouched() {
        let fixture = Fixture::new("rolled-back");
        let (address, server) = response_server(api_error_response("rejected"));
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        server.join().unwrap();

        let ProfileApplyOutcome::Rejected {
            last_known_good,
            cause,
        } = outcome
        else {
            panic!("expected rejected profile outcome");
        };
        assert_eq!(
            last_known_good.map(|version| version.profile_id),
            Some(fixture.previous.id.clone())
        );
        assert!(matches!(cause, ProfileApplicationError::Runtime(_)));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn interrupted_runtime_response_is_reported_as_unknown_without_source_commit() {
        let fixture = Fixture::new("runtime-unknown");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            assert!(bytes > 0);
        });
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        server.join().unwrap();

        let ProfileApplyOutcome::RuntimeUnknown {
            recovery,
            cause,
            runtime_version,
        } = outcome
        else {
            panic!("expected runtime-unknown outcome");
        };
        assert_eq!(runtime_version, 1);
        assert!(matches!(cause, ProfileApplicationError::Runtime(_)));
        assert_eq!(
            recovery.last_known_good.map(|version| version.profile_id),
            Some(fixture.previous.id.clone())
        );
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        assert_eq!(
            fs::read_dir(fixture.store.root().join("staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn missing_profile_is_rejected_without_contacting_the_runtime() {
        let fixture = Fixture::new("missing");
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let application =
            ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: "missing".into(),
                overrides: Vec::new(),
            })
            .await;

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn local_import_uses_the_same_apply_transaction() {
        let fixture = Fixture::new("import-applied");
        let source = fixture.write_source("imported.yaml", "DIRECT");
        let (address, server) =
            response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ImportLocal {
                source,
                overrides: Vec::new(),
            })
            .await;
        server.join().unwrap();

        let ProfileApplyOutcome::Applied { profile, .. } = outcome else {
            panic!("expected applied imported profile");
        };
        let catalog = fixture.store.load().unwrap();
        assert_eq!(catalog.active.as_deref(), Some(profile.id.as_str()));
        assert!(catalog.profiles.iter().any(|item| item.id == profile.id));
    }

    #[tokio::test]
    async fn rejected_local_import_never_commits_the_new_source() {
        let fixture = Fixture::new("import-rejected");
        let source = fixture.write_source("rejected-import.yaml", "REJECT");
        let profiles_before = fixture.store.load().unwrap().profiles.len();
        let (address, server) = response_server(api_error_response("rejected"));
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::ImportLocal {
                source,
                overrides: Vec::new(),
            })
            .await;
        server.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        let catalog = fixture.store.load().unwrap();
        assert_eq!(catalog.profiles.len(), profiles_before);
        assert_eq!(
            catalog.active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn downloaded_source_passes_through_runtime_apply_without_early_persistence() {
        let fixture = Fixture::new("remote-rejected");
        let profiles_before = fixture.store.load().unwrap().profiles.len();
        let (origin_address, origin) =
            response_server(http_ok_response("text/yaml", &profile_payload("REJECT")));
        let (controller_address, controller) = response_server(api_error_response("rejected"));
        let application = fixture.application(controller_address);

        let outcome = application
            .apply(ProfileChange::AddRemote {
                name: String::new(),
                url: format!("http://{origin_address}/profile.yaml"),
                user_agent: "ZenClash-ProfileApplication-Test".into(),
                options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
                overrides: Vec::new(),
            })
            .await;
        origin.join().unwrap();
        controller.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        let catalog = fixture.store.load().unwrap();
        assert_eq!(catalog.profiles.len(), profiles_before);
        assert_eq!(
            catalog.active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn fallback_download_still_requires_final_runtime_acceptance() {
        let fixture = Fixture::new("fallback-download-rejected");
        let profiles_before = fixture.store.load().unwrap().profiles.len();
        let unavailable = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let unavailable_address = unavailable.local_addr().unwrap();
        let proxy = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let proxy_server = thread::spawn(move || {
            let (mut stream, _) = proxy.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            let payload = profile_payload("FALLBACK");
            stream
                .write_all(http_ok_response("text/yaml", &payload).as_bytes())
                .unwrap();
            String::from_utf8_lossy(&request[..bytes]).into_owned()
        });
        let controller = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let controller_address = controller.local_addr().unwrap();
        let controller_server = thread::spawn(move || {
            let mut requests = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = controller.accept().unwrap();
                let mut request = [0_u8; 8_192];
                let bytes = stream.read(&mut request).unwrap();
                requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
                let response = if request_index == 0 {
                    let body = format!(r#"{{"mixed-port":{}}}"#, proxy_address.port());
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    api_error_response("rejected after fallback")
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        let application = fixture.application(controller_address);
        let subscription_url = format!("http://{unavailable_address}/profile.yaml");

        let outcome = application
            .apply(ProfileChange::AddRemote {
                name: "Fallback Candidate".into(),
                url: subscription_url.clone(),
                user_agent: "ZenClash-Fallback-Application-Test".into(),
                options: RemoteProfileOptions::default()
                    .with_download_policy(1, false)
                    .unwrap(),
                overrides: Vec::new(),
            })
            .await;
        drop(unavailable);
        let proxy_request = proxy_server.join().unwrap();
        let controller_requests = controller_server.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert!(proxy_request.starts_with(&format!("GET {subscription_url} HTTP/1.1")));
        assert!(controller_requests[0].starts_with("GET /configs "));
        assert!(controller_requests[1].starts_with("PUT /configs?force=true "));
        let catalog = fixture.store.load().unwrap();
        assert_eq!(catalog.profiles.len(), profiles_before);
        assert_eq!(
            catalog.active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn rejected_active_yaml_edit_preserves_the_source_and_active_revision() {
        let fixture = Fixture::new("edit-rejected");
        let path = fixture.store.profile_path(&fixture.previous);
        let original = fs::read_to_string(&path).unwrap();
        let (address, server) = response_server(api_error_response("rejected"));
        let application = fixture.application(address);

        let outcome = application
            .apply(ProfileChange::EditYaml {
                id: fixture.previous.id.clone(),
                expected_payload: original.clone(),
                new_payload: profile_payload("REJECT"),
                overrides: Vec::new(),
            })
            .await;
        server.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn inactive_remote_update_is_validated_and_stored_without_runtime_apply() {
        let fixture = Fixture::new("inactive-remote-update");
        let (origin_address, origin) =
            response_server(http_ok_response("text/yaml", &profile_payload("UPDATED")));
        let remote = fixture
            .store
            .store_profile(
                "Remote".into(),
                ProfileSource::Remote {
                    url: format!("http://{origin_address}/profile.yaml"),
                    user_agent: "ZenClash-Test".into(),
                    options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
                },
                &profile_payload("OLD"),
            )
            .unwrap();
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let application = ProfileApplication::new(
            fixture.store.clone(),
            fixture.controlled.clone(),
            session.clone(),
        );

        let outcome = application
            .apply(ProfileChange::UpdateRemote {
                id: remote.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        origin.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Stored { .. }));
        assert_eq!(session.snapshot().generation, 0);
        assert!(
            fs::read_to_string(fixture.store.profile_path(&remote))
                .unwrap()
                .contains("MATCH,UPDATED")
        );
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
    }

    #[tokio::test]
    async fn rejected_active_remote_update_preserves_the_downloaded_source_lkg() {
        let fixture = Fixture::new("active-remote-update-rejected");
        let (origin_address, origin) =
            response_server(http_ok_response("text/yaml", &profile_payload("NEW")));
        let remote = fixture
            .store
            .store_profile(
                "Remote".into(),
                ProfileSource::Remote {
                    url: format!("http://{origin_address}/profile.yaml"),
                    user_agent: "ZenClash-Test".into(),
                    options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
                },
                &profile_payload("OLD"),
            )
            .unwrap();
        fixture.store.activate(&remote.id).unwrap();
        let path = fixture.store.profile_path(&remote);
        let original = fs::read_to_string(&path).unwrap();
        let (controller_address, controller) = response_server(api_error_response("rejected"));
        let application = fixture.application(controller_address);

        let outcome = application
            .apply(ProfileChange::UpdateRemote {
                id: remote.id.clone(),
                overrides: Vec::new(),
            })
            .await;
        origin.join().unwrap();
        controller.join().unwrap();

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(remote.id.as_str())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn final_merged_candidate_is_validated_before_runtime_or_persistence() {
        use std::os::unix::fs::PermissionsExt as _;

        use crate::CoreConfigValidator;

        let fixture = Fixture::new("merged-validation-rejected");
        let override_path = fixture.write_source("rejecting-override.yaml", "REJECT_FINAL");
        let validator = fixture.root.join("reject-final.sh");
        fs::write(
            &validator,
            "#!/bin/sh\nfile=''\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = '-f' ]; then shift; file=\"$1\"; fi\n  shift\ndone\nif grep -q 'REJECT_FINAL' \"$file\"; then exit 1; fi\nexit 0\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&validator).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&validator, permissions).unwrap();
        let client = MihomoClient::new(MihomoEndpoint::default())
            .unwrap()
            .with_config_validator(CoreConfigValidator::new(
                CoreKind::Mihomo,
                validator,
                fixture.root.join("validator-home"),
            ))
            .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let application =
            ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![override_path],
            })
            .await;

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        let staging = fixture.store.root().join("staging");
        assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
        assert_eq!(application.session.snapshot().generation, 0);
    }

    #[tokio::test]
    async fn override_parse_failure_only_removes_the_staging_candidate() {
        let fixture = Fixture::new("override-parse-rejected");
        let override_path = fixture.root.join("sources").join("invalid-override.yaml");
        fs::write(&override_path, "rules: [").unwrap();
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let application =
            ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

        let outcome = application
            .apply(ProfileChange::ActivateExisting {
                id: fixture.candidate.id.clone(),
                overrides: vec![override_path],
            })
            .await;

        assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
        assert_eq!(
            fixture.store.load().unwrap().active.as_deref(),
            Some(fixture.previous.id.as_str())
        );
        assert_eq!(
            fs::read_dir(fixture.store.root().join("staging"))
                .unwrap()
                .count(),
            0
        );
        assert!(!fixture.controlled.runtime_path().exists());
    }

    struct Fixture {
        root: PathBuf,
        store: ProfileStore,
        controlled: ControlledConfigStore,
        previous: ProfileRecord,
        candidate: ProfileRecord,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "zenclash-profile-application-{name}-{}",
                std::process::id()
            ));
            let source_root = root.join("sources");
            fs::create_dir_all(&source_root).unwrap();
            let previous_source = source_root.join("previous.yaml");
            let candidate_source = source_root.join("candidate.yaml");
            fs::write(&previous_source, profile_payload("DIRECT")).unwrap();
            fs::write(&candidate_source, profile_payload("REJECT")).unwrap();
            let store = ProfileStore::new(root.join("profiles")).unwrap();
            let previous = store.import_local(previous_source).unwrap();
            let candidate = store.import_local(candidate_source).unwrap();
            store.activate(&previous.id).unwrap();
            let controlled = ControlledConfigStore::new(root.join("controlled"));
            Self {
                root,
                store,
                controlled,
                previous,
                candidate,
            }
        }

        fn application(&self, address: std::net::SocketAddr) -> ProfileApplication {
            let client =
                MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
            let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
            ProfileApplication::new(self.store.clone(), self.controlled.clone(), session)
        }

        fn write_source(&self, name: &str, target: &str) -> PathBuf {
            let path = self.root.join("sources").join(name);
            fs::write(&path, profile_payload(target)).unwrap();
            path
        }
    }

    fn profile_payload(target: &str) -> String {
        format!("mode: rule\nproxies: []\nproxy-groups: []\nrules:\n  - MATCH,{target}\n")
    }

    fn response_server(response: String) -> (std::net::SocketAddr, thread::JoinHandle<String>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&request[..bytes]).into_owned()
        });
        (address, server)
    }

    fn api_error_response(message: &str) -> String {
        let body = format!(r#"{{"message":"{message}"}}"#);
        format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn http_ok_response(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
