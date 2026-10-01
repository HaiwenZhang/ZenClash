//! Versioned local backup and transactional restore of authoritative app data.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    AppPreferencesError, AppPreferencesStore, ControlledConfigError, ControlledConfigStore,
    ProfileStoreError, YamlOverrideError,
};

mod archive;
mod transaction;

#[cfg(test)]
mod tests;

const MANIFEST_PATH: &str = "manifest.json";
const PREFERENCES_PATH: &str = "preferences.json";
const CONTROLLED_PATH: &str = "controlled-config/override.yaml";
const PROFILE_INDEX_PATH: &str = "profiles/profiles.json";
const YAML_OVERRIDE_INDEX_PATH: &str = "yaml-overrides/overrides.json";
const BACKUP_FORMAT_VERSION: u32 = 2;
pub(crate) const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BACKUP_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BACKUP_FILES: usize = 4096;

/// Errors produced while exporting, validating, or restoring a local backup.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BackupError {
    /// Filesystem access failed.
    #[error("备份 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// ZIP encoding or decoding failed.
    #[error("备份 ZIP 无效：{0}")]
    Zip(#[from] zip::result::ZipError),
    /// The backup manifest could not be encoded or decoded.
    #[error("备份清单 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    /// Application preferences in the snapshot are invalid.
    #[error("备份中的应用设置无效：{0}")]
    Preferences(#[from] AppPreferencesError),
    /// Controlled Mihomo configuration in the snapshot is invalid.
    #[error("备份中的受控配置无效：{0}")]
    Controlled(#[from] ControlledConfigError),
    /// Profile catalog or YAML data in the snapshot is invalid.
    #[error("备份中的配置仓库无效：{0}")]
    Profiles(#[from] ProfileStoreError),
    /// Managed YAML override catalog or payload is invalid.
    #[error("备份中的 YAML 覆写无效：{0}")]
    YamlOverrides(#[from] YamlOverrideError),
    /// The archive violates the versioned format or a security constraint.
    #[error("备份内容无效：{0}")]
    InvalidArchive(String),
    /// The archive or its expanded payload exceeded the defensive limit.
    #[error("备份超过安全大小限制：{0}")]
    TooLarge(String),
    /// A platform application-data directory could not be determined.
    #[error("无法确定 ZenClash 应用数据目录")]
    MissingDataDirectory,
    /// Activation or rollback could not be completed consistently.
    #[error("备份恢复事务失败：{0}")]
    Transaction(String),
}

/// Result type returned by local backup operations.
pub type BackupResult<T> = Result<T, BackupError>;

/// Metadata returned after a backup archive is written successfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupExportSummary {
    /// Destination ZIP path.
    pub path: PathBuf,
    /// Number of authoritative data files included in the manifest.
    pub file_count: usize,
    /// Total uncompressed authoritative payload bytes.
    pub payload_bytes: u64,
}

/// Current authoritative data, read after any active restore completes.
#[derive(Clone, Debug)]
pub struct BackupDataSnapshot {
    /// Latest persisted application preferences.
    pub preferences: crate::AppPreferences,
    /// Latest managed-profile index.
    pub profiles: crate::ProfileCatalog,
    /// Latest controlled configuration layer.
    pub controlled_config: serde_json::Value,
    /// Latest ordered YAML override index.
    pub overrides: crate::YamlOverrideCatalog,
}

/// Coordinates local backup export and transactional restore for one data root.
#[derive(Clone, Debug)]
pub struct BackupManager {
    data_root: PathBuf,
}

impl BackupManager {
    /// Opens the platform-default `ZenClash` application-data root.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform data location cannot be determined.
    pub fn discover() -> BackupResult<Self> {
        let preferences = AppPreferencesStore::discover()?;
        let data_root = preferences
            .path()
            .parent()
            .map(Path::to_path_buf)
            .ok_or(BackupError::MissingDataDirectory)?;
        Ok(Self::new(data_root))
    }

    /// Creates a backup manager rooted at an explicit application-data path.
    #[must_use]
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
        }
    }

    /// Returns the application-data root managed by this instance.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Reads authoritative state without observing an unfinished restore.
    ///
    /// Run on a blocking worker. The shared root and physical managed-directory
    /// scopes remain reserved throughout the read; store construction reuses
    /// that authority instead of acquiring a nested lease.
    ///
    /// # Errors
    ///
    /// Returns filesystem or persisted-data decoding errors.
    pub fn read_snapshot(&self) -> BackupResult<BackupDataSnapshot> {
        let mut scopes = transaction::write_scopes(&self.data_root);
        scopes.push(self.data_root.clone());
        let lease = crate::data_coordinator::DataWriteLease::shared(scopes);
        let profiles = crate::ProfileStore::open(self.data_root.join("profiles"), Some(&lease))?;
        let overrides =
            crate::YamlOverrideStore::open(self.data_root.join("yaml-overrides"), Some(&lease))?;
        Ok(BackupDataSnapshot {
            preferences: AppPreferencesStore::new(self.data_root.join(PREFERENCES_PATH)).load()?,
            profiles: profiles.load()?,
            controlled_config: ControlledConfigStore::new(self.data_root.join("controlled-config"))
                .load_json()?,
            overrides: overrides.load()?,
        })
    }

    /// Writes a versioned ZIP containing the current authoritative app data.
    ///
    /// # Errors
    ///
    /// Returns an error when current state is invalid, exceeds limits, or the
    /// destination cannot be written atomically.
    pub fn export_to(&self, destination: impl AsRef<Path>) -> BackupResult<BackupExportSummary> {
        archive::export(self, destination.as_ref())
    }

    /// Validates and stages a backup without changing live application data.
    ///
    /// The returned value must be activated explicitly. Archive paths, file
    /// types, sizes, manifest checksums, preferences, and all profile YAML are
    /// validated before this method succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, unsafe, incompatible, or invalid data.
    pub fn prepare_restore(
        &self,
        archive: impl AsRef<Path>,
    ) -> BackupResult<PreparedBackupRestore> {
        archive::prepare_restore(self, archive.as_ref())
    }
}

/// A completely validated backup extracted into an isolated staging directory.
#[derive(Debug)]
pub struct PreparedBackupRestore {
    data_root: PathBuf,
    staging_root: PathBuf,
    file_count: usize,
    payload_bytes: u64,
}

impl PreparedBackupRestore {
    /// Number of authoritative files validated in the archive.
    #[must_use]
    pub const fn file_count(&self) -> usize {
        self.file_count
    }

    /// Total uncompressed authoritative payload bytes.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Atomically swaps staged authoritative data into the live root.
    ///
    /// The returned transaction automatically restores the previous snapshot
    /// unless [`BackupRestoreTransaction::commit`] is called.
    ///
    /// # Errors
    ///
    /// Returns an error when activation or its immediate rollback fails.
    pub fn activate(self) -> BackupResult<BackupRestoreTransaction> {
        transaction::activate(self, None)
    }

    /// Activates a restore while also reserving the session's actual runtime paths.
    ///
    /// Runtime homes can be outside the restored root. All scopes are acquired
    /// together before any swap and retained through commit or rollback. Call
    /// from a blocking worker, as activation may wait for existing writers.
    ///
    /// # Errors
    ///
    /// Returns an error when activation or its immediate rollback fails.
    pub fn activate_for_session(
        self,
        session: &crate::CoreSession,
    ) -> BackupResult<BackupRestoreTransaction> {
        transaction::activate(self, Some(session))
    }
}

impl Drop for PreparedBackupRestore {
    fn drop(&mut self) {
        if self.staging_root.exists()
            && let Err(error) = std::fs::remove_dir_all(&self.staging_root)
        {
            tracing::warn!(%error, path = %self.staging_root.display(), "failed to remove backup staging directory");
        }
    }
}

/// Reversible activation of an imported backup snapshot.
pub struct BackupRestoreTransaction {
    data_root: PathBuf,
    rollback_root: PathBuf,
    remove_empty_data_root: bool,
    installed: Vec<&'static str>,
    preserved: Vec<&'static str>,
    active: bool,
    previous_runtime: Option<crate::CoreRestoreSnapshot>,
    lease: crate::data_coordinator::DataWriteLease,
}

impl std::fmt::Debug for BackupRestoreTransaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackupRestoreTransaction")
            .field("data_root", &self.data_root)
            .field("rollback_root", &self.rollback_root)
            .field("active", &self.active)
            .field("has_previous_runtime", &self.previous_runtime.is_some())
            .finish_non_exhaustive()
    }
}

impl BackupRestoreTransaction {
    /// Returns the committed runtime captured after exclusion and before disk activation.
    ///
    /// Present only for [`PreparedBackupRestore::activate_for_session`]. The
    /// snapshot replays exact accepted configuration without rereading source YAML.
    #[must_use]
    pub fn previous_runtime_snapshot(&self) -> Option<crate::CoreRestoreSnapshot> {
        self.previous_runtime.clone()
    }

    /// Opens the restored profile store under this transaction's exclusive authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed directory cannot be initialized.
    pub fn profile_store(&self) -> BackupResult<crate::ProfileStore> {
        Ok(crate::ProfileStore::open(
            self.data_root.join("profiles"),
            Some(&self.lease),
        )?)
    }

    /// Opens restored overrides under this transaction's exclusive authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed directory cannot be initialized.
    pub fn yaml_override_store(&self) -> BackupResult<crate::YamlOverrideStore> {
        Ok(crate::YamlOverrideStore::open(
            self.data_root.join("yaml-overrides"),
            Some(&self.lease),
        )?)
    }

    /// Authorizes controlled-cache writes while the restored runtime is validated.
    /// The returned store's authority expires when the transaction is consumed or dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is outside the restored data root.
    pub fn authorize_controlled_store(
        &self,
        store: crate::ControlledConfigStore,
    ) -> BackupResult<crate::ControlledConfigStore> {
        if !crate::data_coordinator::path_within(store.root(), &self.data_root)
            || !self.lease.covers(store.root())
        {
            return Err(BackupError::Transaction(
                "受控配置不属于此次恢复的数据目录".into(),
            ));
        }
        Ok(store.with_write_lease(&self.lease))
    }

    /// Keeps the restored snapshot and removes the previous-data rollback copy.
    ///
    /// # Errors
    ///
    /// Returns an error when the now-unneeded rollback directory cannot be
    /// removed. The imported snapshot remains active in this case.
    pub fn commit(mut self) -> BackupResult<()> {
        self.active = false;
        if self.rollback_root.exists() {
            std::fs::remove_dir_all(&self.rollback_root)?;
        }
        Ok(())
    }

    /// Restores the complete authoritative snapshot that preceded activation.
    /// Successfully restored items are retained if a later step fails and
    /// dropping the transaction retries its remaining work.
    ///
    /// # Errors
    ///
    /// Returns an error when any live item cannot be removed or restored, or
    /// when the empty transaction directories cannot be removed afterward.
    pub fn rollback(mut self) -> BackupResult<()> {
        self.rollback_in_place()
    }

    /// Restores the preceding disk snapshot while retaining exclusive write authority.
    ///
    /// Use this after the runtime accepted imported data: reconcile the previous
    /// runtime through this transaction's authorized stores before dropping it.
    /// A successful disk rollback disables automatic rollback, but ordinary
    /// writers remain blocked until this transaction is consumed or dropped.
    ///
    /// # Errors
    ///
    /// Returns a removal, restoration, or cleanup error. Successful moves are
    /// retained, so a retry or drop only performs the remaining rollback work.
    pub fn rollback_in_place(&mut self) -> BackupResult<()> {
        if self.active {
            transaction::rollback(self)?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for BackupRestoreTransaction {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = transaction::rollback(self)
        {
            tracing::error!(%error, "failed to roll back uncommitted backup restore");
        }
    }
}
