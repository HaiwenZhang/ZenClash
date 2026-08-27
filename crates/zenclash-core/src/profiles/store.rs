use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;

use super::{
    ProfileCatalog, ProfileRecord, ProfileSource, ProfileStore, ProfileStoreError,
    ProfileStoreResult, SubscriptionMetadata, atomic_write, home_dir, read_index_bytes,
    read_profile_bytes, unique_id, unix_timestamp, validate_clash_yaml,
};

impl ProfileStore {
    /// Opens the platform-default profile directory, creating it when needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the user's data directory cannot be determined or
    /// the profile directory cannot be created.
    pub fn discover() -> ProfileStoreResult<Self> {
        let root = if cfg!(target_os = "macos") {
            home_dir()?.join("Library/Application Support/ZenClash/profiles")
        } else if cfg!(target_os = "windows") {
            if let Some(data_home) = std::env::var_os("LOCALAPPDATA") {
                PathBuf::from(data_home).join("ZenClash/profiles")
            } else {
                home_dir()?.join("AppData/Local/ZenClash/profiles")
            }
        } else if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            PathBuf::from(data_home).join("zenclash/profiles")
        } else {
            home_dir()?.join(".local/share/zenclash/profiles")
        };
        Self::new(root)
    }

    /// Opens a profile store rooted at `root`, creating its files directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> ProfileStoreResult<Self> {
        let store = Self {
            root: root.into(),
            transaction: Arc::new(Mutex::new(())),
        };
        fs::create_dir_all(store.files_dir())?;
        Ok(store)
    }

    /// Returns the root directory containing the index and managed files.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Quarantines a malformed profile index while retaining managed YAML files.
    ///
    /// Only JSON decoding and defensive-size failures are recoverable. I/O
    /// failures are returned unchanged so a permission problem is never
    /// presented as repaired state.
    ///
    /// # Errors
    ///
    /// Returns the original non-recoverable error or an I/O error while moving
    /// the invalid index to its timestamped backup.
    pub fn quarantine_invalid_index(&self) -> ProfileStoreResult<Option<PathBuf>> {
        let _transaction = self.transaction.lock();
        let error = match self.load_unlocked() {
            Ok(_) => return Ok(None),
            Err(error) => error,
        };
        if !matches!(
            error,
            ProfileStoreError::Index(_) | ProfileStoreError::IndexTooLarge { .. }
        ) {
            return Err(error);
        }
        let source = self.index_path();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine = self.root.join(format!(
            "profiles.invalid-{}-{timestamp}.json",
            std::process::id()
        ));
        fs::rename(source, &quarantine)?;
        Ok(Some(quarantine))
    }

    /// Loads and repairs the in-memory catalog view.
    ///
    /// Records whose managed YAML files are missing are omitted. A dangling
    /// active profile is cleared from the returned view.
    ///
    /// # Errors
    ///
    /// Returns an error when the index cannot be read or decoded.
    pub fn load(&self) -> ProfileStoreResult<ProfileCatalog> {
        let _transaction = self.transaction.lock();
        self.load_unlocked()
    }

    /// Resolves the active profile to its managed YAML path.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog cannot be loaded.
    pub fn active_path(&self) -> ProfileStoreResult<Option<PathBuf>> {
        let _transaction = self.transaction.lock();
        let catalog = self.load_unlocked()?;
        Ok(catalog
            .active_profile()
            .map(|profile| self.profile_path(profile)))
    }

    /// Imports and validates a local Clash/Mihomo YAML file.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, oversized, non-UTF-8, or invalid YAML
    /// input, or when the managed profile cannot be persisted.
    pub fn import_local(&self, path: impl AsRef<Path>) -> ProfileStoreResult<ProfileRecord> {
        let path = path.as_ref();
        let payload = String::from_utf8(read_profile_bytes(path)?).map_err(|error| {
            ProfileStoreError::InvalidYaml(format!("本地配置内容不是 UTF-8：{error}"))
        })?;
        validate_clash_yaml(&payload)?;
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("本地配置")
            .to_string();
        self.store_profile(
            name,
            ProfileSource::Local {
                original_path: path.display().to_string(),
            },
            &payload,
        )
    }

    /// Returns the managed YAML path for a profile record.
    #[must_use]
    pub fn profile_path(&self, profile: &ProfileRecord) -> PathBuf {
        self.files_dir().join(&profile.file_name)
    }

    pub(super) fn store_profile(
        &self,
        name: String,
        source: ProfileSource,
        payload: &str,
    ) -> ProfileStoreResult<ProfileRecord> {
        self.store_profile_with_subscription(name, source, payload, SubscriptionMetadata::default())
    }

    pub(super) fn store_profile_with_subscription(
        &self,
        name: String,
        source: ProfileSource,
        payload: &str,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<ProfileRecord> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let id = unique_id(&catalog, &name);
        let file_name = format!("{id}.yaml");
        let accepts_suggested_interval = matches!(
            &source,
            ProfileSource::Remote { options, .. } if !options.fixed_update_interval
        );
        let update_interval_minutes = accepts_suggested_interval
            .then_some(subscription.suggested_update_interval_minutes)
            .flatten()
            .unwrap_or(super::DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES);
        let record = ProfileRecord {
            id,
            name,
            file_name,
            source,
            updated_at: unix_timestamp(),
            size_bytes: payload.len() as u64,
            auto_update: false,
            update_interval_minutes,
            update_cron: None,
            subscription,
        };
        let path = self.profile_path(&record);
        atomic_write(&path, payload.as_bytes())?;
        catalog.profiles.push(record.clone());
        if let Err(error) = self.save_unlocked(&catalog) {
            return match fs::remove_file(&path) {
                Ok(()) => Err(error),
                Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                    "保存配置索引失败：{error}；清理未入库配置失败：{rollback}"
                ))),
            };
        }
        Ok(record)
    }

    pub(super) fn load_unlocked(&self) -> ProfileStoreResult<ProfileCatalog> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(ProfileCatalog::default());
        }
        let contents = read_index_bytes(&path)?;
        let mut catalog: ProfileCatalog = serde_json::from_slice(&contents)?;
        catalog
            .profiles
            .retain(|profile| self.profile_path(profile).is_file());
        if catalog
            .active
            .as_ref()
            .is_some_and(|active| !catalog.profiles.iter().any(|profile| &profile.id == active))
        {
            catalog.active = None;
        }
        Ok(catalog)
    }

    pub(super) fn save_unlocked(&self, catalog: &ProfileCatalog) -> ProfileStoreResult<()> {
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec_pretty(catalog)?;
        let path = self.index_path();
        atomic_write(&path, &bytes)?;
        Ok(())
    }

    fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("profiles.json")
    }
}
