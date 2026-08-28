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
use crate::{ControlledConfigStore, CoreApplyKind, CoreSession, CoreSessionError, MihomoError};

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
    /// A blocking repository task ended unexpectedly.
    #[error("配置事务后台任务异常结束：{0}")]
    Task(String),
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
    },
    /// The source was not committed, but a transport failure obscured runtime truth.
    RuntimeUnknown {
        /// Candidate and last-known-good revisions needed for reconciliation.
        recovery: ProfileRecovery,
        /// Transport or recovery failure that made runtime state uncertain.
        cause: ProfileApplicationError,
    },
    /// Persistence may be partial and runtime rollback also failed.
    PersistedButRuntimeUnknown {
        /// Information required to offer a targeted recovery action.
        recovery: ProfileRecovery,
        /// Persistence failure that triggered the rollback attempt.
        cause: ProfileApplicationError,
        /// Runtime recovery failure that left effective state uncertain.
        rollback: ProfileApplicationError,
    },
}

/// Cloneable owner of managed-profile and runtime application ordering.
#[derive(Clone)]
pub struct ProfileApplication {
    store: ProfileStore,
    controlled: ControlledConfigStore,
    session: CoreSession,
}

struct StagedProfile {
    store: ProfileStore,
    record: ProfileRecord,
    candidate_path: PathBuf,
    expected_catalog: ProfileCatalog,
    disposition: StagedProfileDisposition,
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

impl StagedProfile {
    fn existing(store: ProfileStore, id: &str) -> ProfileStoreResult<Self> {
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
        let candidate_path = write_staging_payload(store.root(), &payload)?;
        drop(_transaction);
        Ok(Self {
            store,
            record,
            candidate_path,
            expected_catalog: catalog,
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
        let _transaction = store.transaction.lock();
        let catalog = store.load_unlocked()?;
        let record =
            ProfileStore::new_profile_record(&catalog, name, source, payload.len(), subscription);
        let candidate_path = write_staging_payload(store.root(), payload.as_bytes())?;
        drop(_transaction);
        Ok(Self {
            store,
            record,
            candidate_path,
            expected_catalog: catalog,
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
        Ok(Self {
            store,
            record,
            candidate_path,
            expected_catalog: catalog,
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
        Ok(Self {
            store,
            record,
            candidate_path,
            expected_catalog: catalog,
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

    fn candidate_path(&self) -> &Path {
        &self.candidate_path
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

    fn commit(self) -> ProfileStoreResult<CommittedProfile> {
        let _transaction = self.store.transaction.lock();
        let mut catalog = self.store.load_unlocked()?;
        if catalog != self.expected_catalog {
            return Err(ProfileStoreError::Transaction(
                "配置目录在候选验证期间已改变，请刷新后重试".into(),
            ));
        }

        match &self.disposition {
            StagedProfileDisposition::ExistingActivation {
                source_path,
                expected_payload,
            } => {
                if read_profile_bytes(source_path)? != *expected_payload {
                    return Err(ProfileStoreError::Transaction(format!(
                        "配置 {} 在候选验证期间已改变，请刷新后重试",
                        self.record.id
                    )));
                }
                catalog.active = Some(self.record.id.clone());
                self.store.save_unlocked(&catalog)?;
                Ok(CommittedProfile {
                    record: self.record.clone(),
                    path: source_path.clone(),
                })
            }
            StagedProfileDisposition::New => {
                let payload = read_profile_bytes(&self.candidate_path)?;
                let path = self.store.profile_path(&self.record);
                atomic_write(&path, &payload)?;
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
                if read_profile_bytes(source_path)? != *expected_payload {
                    return Err(ProfileStoreError::Transaction(format!(
                        "配置 {} 在候选验证期间已改变，请刷新后重试",
                        self.record.id
                    )));
                }
                let index = catalog
                    .profiles
                    .iter()
                    .position(|profile| profile.id == self.record.id)
                    .ok_or_else(|| ProfileStoreError::NotFound(self.record.id.clone()))?;
                let payload = read_profile_bytes(&self.candidate_path)?;
                atomic_write(source_path, &payload)?;
                catalog.profiles[index] = self.record.clone();
                if let Err(error) = self.store.save_unlocked(&catalog) {
                    return match atomic_write(source_path, expected_payload) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                            "保存配置索引失败：{error}；恢复上一版本配置失败：{rollback}"
                        ))),
                    };
                }
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
        let _ = fs::remove_file(&self.candidate_path);
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
        }
    }

    /// Applies one managed-profile change and classifies its recovery state.
    pub async fn apply(&self, change: ProfileChange) -> ProfileApplyOutcome {
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

    async fn import_local(&self, source: PathBuf, overrides: Vec<PathBuf>) -> ProfileApplyOutcome {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good: None,
                    cause,
                };
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || StagedProfile::local(store, &source)).await {
            Ok(staged) => staged,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        self.apply_staged(staged, overrides).await
    }

    async fn update_remote(&self, id: String, overrides: Vec<PathBuf>) -> ProfileApplyOutcome {
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
                return ProfileApplyOutcome::Rejected {
                    last_known_good: None,
                    cause,
                };
            }
        };
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &expected_record.source
        else {
            return ProfileApplyOutcome::Rejected {
                last_known_good,
                cause: ProfileStoreError::NotFound(format!("{id} 不是在线订阅")).into(),
            };
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        let downloaded = match download_profile(url, user_agent, options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: error.into(),
                };
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
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        self.apply_staged(staged, overrides).await
    }

    async fn edit_yaml(
        &self,
        id: String,
        expected_payload: String,
        new_payload: String,
        overrides: Vec<PathBuf>,
    ) -> ProfileApplyOutcome {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good: None,
                    cause,
                };
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
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        self.apply_staged(staged, overrides).await
    }

    async fn add_remote(
        &self,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
        overrides: Vec<PathBuf>,
    ) -> ProfileApplyOutcome {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good: None,
                    cause,
                };
            }
        };
        let name = match normalized_profile_name(&name) {
            Ok(name) => name,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: error.into(),
                };
            }
        };
        let url = match normalized_remote_url(&url) {
            Ok(url) => url,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: error.into(),
                };
            }
        };
        let user_agent = match normalized_user_agent(&user_agent) {
            Ok(user_agent) => user_agent,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: error.into(),
                };
            }
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        let downloaded = match download_profile(&url, &user_agent, &options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: error.into(),
                };
            }
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
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        self.apply_staged(staged, overrides).await
    }

    async fn activate_existing(&self, id: String, overrides: Vec<PathBuf>) -> ProfileApplyOutcome {
        let last_known_good = match self.last_known_good().await {
            Ok(version) => version,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good: None,
                    cause,
                };
            }
        };
        let store = self.store.clone();
        let staged = match run_store(move || StagedProfile::existing(store, &id)).await {
            Ok(staged) => staged,
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                };
            }
        };
        self.apply_staged(staged, overrides).await
    }

    async fn apply_staged(
        &self,
        staged: StagedProfile,
        overrides: Vec<PathBuf>,
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
            .stage_profile_application(
                &self.controlled,
                staged.candidate_path().to_path_buf(),
                staged.previous_path(),
                overrides,
                changes_runtime,
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(cause) if core_error_runtime_unknown(&cause) => {
                return ProfileApplyOutcome::RuntimeUnknown {
                    recovery,
                    cause: cause.into(),
                };
            }
            Err(cause) => {
                return ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: cause.into(),
                };
            }
        };

        let commit = run_store(move || staged.commit()).await;
        match commit {
            Ok(committed) => match runtime.commit() {
                Some(applied) => ProfileApplyOutcome::Applied {
                    profile: committed.record,
                    path: committed.path,
                    source_version,
                    runtime_version: applied.generation,
                    kind: applied.kind,
                },
                None => ProfileApplyOutcome::Stored {
                    profile: committed.record,
                    path: committed.path,
                    source_version,
                },
            },
            Err(cause) => match runtime.rollback().await {
                Ok(()) if changes_runtime => ProfileApplyOutcome::RolledBack {
                    last_known_good,
                    cause,
                },
                Ok(()) => ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause,
                },
                Err(rollback) => ProfileApplyOutcome::PersistedButRuntimeUnknown {
                    recovery,
                    cause,
                    rollback: rollback.into(),
                },
            },
        }
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

fn core_error_runtime_unknown(error: &CoreSessionError) -> bool {
    matches!(
        error,
        CoreSessionError::Config(crate::ControlledConfigError::Profile(MihomoError::Http(_)))
            | CoreSessionError::Config(crate::ControlledConfigError::Transaction(_))
    )
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

        assert!(matches!(outcome, ProfileApplyOutcome::RolledBack { .. }));
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

        let ProfileApplyOutcome::RuntimeUnknown { recovery, cause } = outcome else {
            panic!("expected runtime-unknown outcome");
        };
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
        let session = CoreSession::open(CoreKind::Mihomo, client, None);
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
                name: "Remote Candidate".into(),
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
        let session = CoreSession::open(CoreKind::Mihomo, client, None);
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
            ));
        let session = CoreSession::open(CoreKind::Mihomo, client, None);
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
        let session = CoreSession::open(CoreKind::Mihomo, client, None);
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
            let session = CoreSession::open(CoreKind::Mihomo, client, None);
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
