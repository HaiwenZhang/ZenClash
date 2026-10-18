//! Source candidate files, catalog/source checks and owned staging cleanup.

use super::*;

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct ProfileSourceCandidate {
    store: ProfileStore,
    record: ProfileRecord,
    staging_path: Option<PathBuf>,
    payload: std::sync::Arc<str>,
    expected_catalog: ProfileCatalog,
    disposition: SourceCommitPlan,
    _write_lease: Option<crate::data_coordinator::DataWriteLease>,
}

enum SourceCommitPlan {
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

pub(super) struct CommittedProfile {
    pub(super) record: ProfileRecord,
    pub(super) path: PathBuf,
}

impl ProfileSourceCandidate {
    pub(super) fn store(&self) -> &ProfileStore {
        &self.store
    }

    pub(super) fn payload(&self) -> &std::sync::Arc<str> {
        &self.payload
    }

    pub(super) fn staging_path(&self) -> Option<&Path> {
        self.staging_path.as_deref()
    }

    pub(super) fn into_held_candidate(
        mut self,
        ordinary_store: ProfileStore,
    ) -> ProfileStoreResult<Self> {
        if let Some(path) = &self.staging_path {
            fs::remove_file(path)?;
        }
        self.staging_path = None;
        self.store = ordinary_store;
        self._write_lease = None;
        Ok(self)
    }

    pub(super) fn materialize_for_trial(&mut self, store: ProfileStore) -> ProfileStoreResult<()> {
        self.attach_write_authority(store)?;
        self.validate_source_current()?;
        self.materialize_payload()
    }

    // The Service participant has already checked the source under its outer
    // lease. Commit rechecks catalog and bytes before changing persistent data.
    pub(super) fn materialize_for_commit(&mut self, store: ProfileStore) -> ProfileStoreResult<()> {
        self.attach_write_authority(store)?;
        self.materialize_payload()
    }

    fn attach_write_authority(&mut self, store: ProfileStore) -> ProfileStoreResult<()> {
        self.store = store;
        self._write_lease = Some(self.store.write_access.acquire()?);
        Ok(())
    }

    fn materialize_payload(&mut self) -> ProfileStoreResult<()> {
        self.staging_path = Some(write_staging_payload(
            self.store.root(),
            self.payload.as_bytes(),
        )?);
        Ok(())
    }

    pub(super) fn existing(store: ProfileStore, id: &str) -> ProfileStoreResult<Self> {
        let _write_lease = store.write_access.acquire()?;
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
            staging_path: Some(candidate_path),
            payload: frozen,
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            disposition: SourceCommitPlan::ExistingActivation {
                source_path,
                expected_payload: payload,
            },
        })
    }

    pub(super) fn local(store: ProfileStore, source: &Path) -> ProfileStoreResult<Self> {
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

    pub(super) fn new(
        store: ProfileStore,
        name: String,
        source: ProfileSource,
        payload: String,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&payload)?;
        let _write_lease = store.write_access.acquire()?;
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
            staging_path: Some(candidate_path),
            payload: payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            disposition: SourceCommitPlan::New,
        })
    }

    pub(super) fn remote_update(
        store: ProfileStore,
        expected_record: &ProfileRecord,
        payload: String,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&payload)?;
        let _write_lease = store.write_access.acquire()?;
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
        record.apply_payload_metadata(
            super::super::unix_timestamp(),
            payload.len() as u64,
            subscription,
        );
        let candidate_path = write_staging_payload(store.root(), payload.as_bytes())?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            staging_path: Some(candidate_path),
            payload: payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            disposition: SourceCommitPlan::Update {
                source_path,
                expected_payload,
            },
        })
    }

    pub(super) fn edit(
        store: ProfileStore,
        id: &str,
        expected_payload: &str,
        new_payload: String,
    ) -> ProfileStoreResult<Self> {
        validate_clash_yaml(&new_payload)?;
        let _write_lease = store.write_access.acquire()?;
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
        record.updated_at = super::super::unix_timestamp();
        record.size_bytes = new_payload.len() as u64;
        let candidate_path = write_staging_payload(store.root(), new_payload.as_bytes())?;
        drop(_transaction);
        let store = store.with_write_lease(&_write_lease);
        Ok(Self {
            store,
            record,
            staging_path: Some(candidate_path),
            payload: new_payload.into(),
            expected_catalog: catalog,
            _write_lease: Some(_write_lease),
            disposition: SourceCommitPlan::Update {
                source_path,
                expected_payload: persisted_payload,
            },
        })
    }

    pub(super) fn last_known_good(&self) -> Option<ProfileSourceMetadata> {
        self.expected_catalog
            .active_profile()
            .map(ProfileSourceMetadata::from)
    }

    pub(super) fn previous_path(&self) -> Option<PathBuf> {
        self.expected_catalog
            .active_profile()
            .map(|profile| self.store.profile_path(profile))
    }

    pub(super) fn record(&self) -> &ProfileRecord {
        &self.record
    }

    pub(super) fn changes_runtime(&self) -> bool {
        match &self.disposition {
            SourceCommitPlan::ExistingActivation { .. } | SourceCommitPlan::New => true,
            SourceCommitPlan::Update { .. } => {
                self.expected_catalog.active.as_deref() == Some(self.record.id.as_str())
            }
        }
    }

    pub(super) fn validate_source_current(&self) -> ProfileStoreResult<()> {
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
            SourceCommitPlan::ExistingActivation {
                source_path,
                expected_payload,
            }
            | SourceCommitPlan::Update {
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
            SourceCommitPlan::New => {}
        }
        Ok(())
    }

    pub(super) fn commit(self) -> ProfileStoreResult<CommittedProfile> {
        let _write_lease = self.store.write_access.acquire()?;
        let _transaction = self.store.transaction.lock();
        let mut catalog = self.store.load_unlocked()?;
        if read_profile_bytes(self.staging_path().ok_or_else(|| {
            ProfileStoreError::Transaction(zenclash_i18n::text("profiles.errors.candidate_changed"))
        })?)?
        .as_slice()
            != self.payload.as_bytes()
        {
            return Err(ProfileStoreError::Transaction(zenclash_i18n::text(
                "profiles.errors.candidate_changed",
            )));
        }
        self.validate_source_against_catalog(&catalog)?;

        match &self.disposition {
            SourceCommitPlan::ExistingActivation {
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
            SourceCommitPlan::New => {
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
            SourceCommitPlan::Update {
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

impl Drop for ProfileSourceCandidate {
    fn drop(&mut self) {
        if let Some(path) = &self.staging_path {
            let _ = fs::remove_file(path);
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
