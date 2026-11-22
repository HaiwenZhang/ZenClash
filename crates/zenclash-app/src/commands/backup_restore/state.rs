use std::path::{Path, PathBuf};

use zenclash_core::{
    BackupDataSnapshot, BackupManager, BackupRestoreTransaction, ControlledConfigStore,
    ProfileStore, RuntimeConfig, YamlOverrideStore,
};

#[derive(Debug)]
struct RestoredRepositories {
    profiles: ProfileStore,
    controlled_config: ControlledConfigStore,
    yaml_overrides: YamlOverrideStore,
}

/// Receipt of a completed backup restore carrying restored data facts, the
/// accepted runtime generation, and any non-fatal cleanup warning.
#[derive(Debug)]
pub struct BackupRestoreReceipt {
    manager: BackupManager,
    snapshot: BackupDataSnapshot,
    preferences_observation: zenclash_core::AppPreferencesObservation,
    repositories: RestoredRepositories,
    runtime_generation: u64,
    runtime_config: Option<RuntimeConfig>,
    restored_file_count: usize,
    restored_payload_bytes: u64,
    cleanup_warning: Option<String>,
}

impl BackupRestoreReceipt {
    /// Returns the restored profile store with live write authority.
    #[must_use]
    pub fn profile_store(&self) -> &ProfileStore {
        &self.repositories.profiles
    }

    /// Returns the restored controlled-configuration store.
    #[must_use]
    pub fn controlled_store(&self) -> &ControlledConfigStore {
        &self.repositories.controlled_config
    }

    /// Returns the restored YAML override store.
    #[must_use]
    pub fn override_store(&self) -> &YamlOverrideStore {
        &self.repositories.yaml_overrides
    }

    /// Returns the runtime generation accepted by the core for this restore.
    #[must_use]
    pub fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }

    /// Returns the optional controller runtime configuration observed after restore.
    #[must_use]
    pub fn runtime_config(&self) -> Option<RuntimeConfig> {
        self.runtime_config.clone()
    }

    /// Returns the number of files restored from the archive.
    #[must_use]
    pub fn restored_file_count(&self) -> usize {
        self.restored_file_count
    }

    /// Returns the total restored payload size in bytes.
    #[must_use]
    pub fn restored_payload_bytes(&self) -> u64 {
        self.restored_payload_bytes
    }

    /// Returns any non-fatal cleanup warning produced while finalizing the restore.
    #[must_use]
    pub fn cleanup_warning(&self) -> Option<&str> {
        self.cleanup_warning.as_deref()
    }

    /// Returns the observed preferences snapshot read from the restored data.
    #[must_use]
    pub fn preferences_observation(&self) -> &zenclash_core::AppPreferencesObservation {
        &self.preferences_observation
    }

    /// Consumes the receipt and returns the restored backup data snapshot.
    #[must_use]
    pub fn into_snapshot(self) -> BackupDataSnapshot {
        self.snapshot
    }

    /// Reads current facts for delayed publication without re-running the restore.
    ///
    /// Run on a blocking worker; the page mutation stays reserved until adoption.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the current on-disk backup snapshot fails.
    pub fn refresh_snapshot(mut self) -> Result<Self, String> {
        let (snapshot, observation) = self
            .manager
            .read_observed_snapshot()
            .map_err(|error| error.to_string())?
            .into_parts();
        self.snapshot = snapshot;
        self.preferences_observation = observation;
        Ok(self)
    }

    /// Constructs a synthetic committed backup-restore receipt for UI tests.
    ///
    /// # Panics
    ///
    /// Panics if the test data root cannot open its profile/override stores or
    /// read an observed snapshot.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn committed_for_test(manager: BackupManager, runtime_generation: u64) -> Self {
        let root = manager.data_root();
        let repositories = RestoredRepositories {
            profiles: ProfileStore::new(root.join("profiles")).unwrap(),
            controlled_config: ControlledConfigStore::new(root.join("controlled-config")),
            yaml_overrides: YamlOverrideStore::new(root.join("yaml-overrides")).unwrap(),
        };
        let (snapshot, preferences_observation) =
            manager.read_observed_snapshot().unwrap().into_parts();
        Self {
            snapshot,
            preferences_observation,
            manager,
            repositories,
            runtime_generation,
            runtime_config: None,
            restored_file_count: 1,
            restored_payload_bytes: 1,
            cleanup_warning: None,
        }
    }
}

/// Receipt of a retried backup runtime recovery.
#[derive(Debug)]
pub struct BackupRecoveryReceipt {
    runtime_generation: u64,
    snapshot: Result<zenclash_core::ObservedBackupSnapshot, String>,
}

impl BackupRecoveryReceipt {
    pub(super) fn new(
        runtime_generation: u64,
        snapshot: Result<zenclash_core::ObservedBackupSnapshot, String>,
    ) -> Self {
        Self {
            runtime_generation,
            snapshot,
        }
    }

    /// Returns the runtime generation accepted by the retry.
    #[must_use]
    pub fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }

    /// Consumes the receipt and returns the observed on-disk snapshot readback.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the post-recovery snapshot failed.
    pub fn into_observed_snapshot(self) -> Result<zenclash_core::ObservedBackupSnapshot, String> {
        self.snapshot
    }
}

pub(super) struct LoadedBackupState {
    snapshot: BackupDataSnapshot,
    preferences_observation: zenclash_core::AppPreferencesObservation,
    repositories: RestoredRepositories,
    active_profile_path: PathBuf,
}

impl LoadedBackupState {
    pub(super) fn read(
        root: &Path,
        transaction: &BackupRestoreTransaction,
    ) -> Result<Self, String> {
        let profiles = transaction
            .profile_store()
            .map_err(|error| error.to_string())?;
        let profile_catalog = profiles.load().map_err(|error| error.to_string())?;
        let active_profile_path = profiles
            .active_path()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| zenclash_i18n::text("backup.errors.missing_profile"))?;
        let controlled_config = transaction
            .authorize_controlled_store(ControlledConfigStore::new(root.join("controlled-config")))
            .map_err(|error| error.to_string())?;
        let yaml_overrides = transaction
            .yaml_override_store()
            .map_err(|error| error.to_string())?;
        let preferences_observation = transaction
            .observe_preferences()
            .map_err(|error| error.to_string())?;
        let snapshot = BackupDataSnapshot {
            preferences: preferences_observation.preferences().clone(),
            profiles: profile_catalog,
            controlled_config: controlled_config
                .load_json()
                .map_err(|error| error.to_string())?,
            overrides: yaml_overrides.load().map_err(|error| error.to_string())?,
        };
        Ok(Self {
            snapshot,
            preferences_observation,
            repositories: RestoredRepositories {
                profiles,
                controlled_config,
                yaml_overrides,
            },
            active_profile_path,
        })
    }
    pub(super) fn controlled_store(&self) -> &ControlledConfigStore {
        &self.repositories.controlled_config
    }
    pub(super) fn active_profile_path(&self) -> &Path {
        &self.active_profile_path
    }
    pub(super) fn enabled_overrides(&self) -> Vec<PathBuf> {
        self.repositories
            .yaml_overrides
            .enabled_paths(&self.snapshot.overrides)
    }
    pub(super) fn committed(
        self,
        manager: BackupManager,
        runtime_generation: u64,
        runtime_config: Option<RuntimeConfig>,
        restored_file_count: usize,
        restored_payload_bytes: u64,
        cleanup_warning: Option<String>,
    ) -> BackupRestoreReceipt {
        BackupRestoreReceipt {
            manager,
            snapshot: self.snapshot,
            preferences_observation: self.preferences_observation,
            repositories: self.repositories,
            runtime_generation,
            runtime_config,
            restored_file_count,
            restored_payload_bytes,
            cleanup_warning,
        }
    }
}
