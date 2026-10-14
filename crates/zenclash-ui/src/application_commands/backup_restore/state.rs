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

#[derive(Debug)]
pub(crate) struct BackupRestoreReceipt {
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
    pub(crate) fn profile_store(&self) -> &ProfileStore {
        &self.repositories.profiles
    }
    pub(crate) fn controlled_store(&self) -> &ControlledConfigStore {
        &self.repositories.controlled_config
    }
    pub(crate) fn override_store(&self) -> &YamlOverrideStore {
        &self.repositories.yaml_overrides
    }
    pub(crate) fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }
    pub(crate) fn runtime_config(&self) -> Option<RuntimeConfig> {
        self.runtime_config.clone()
    }
    pub(crate) fn restored_file_count(&self) -> usize {
        self.restored_file_count
    }
    pub(crate) fn restored_payload_bytes(&self) -> u64 {
        self.restored_payload_bytes
    }
    pub(crate) fn cleanup_warning(&self) -> Option<&str> {
        self.cleanup_warning.as_deref()
    }
    pub(crate) fn preferences_observation(&self) -> &zenclash_core::AppPreferencesObservation {
        &self.preferences_observation
    }
    pub(crate) fn into_snapshot(self) -> BackupDataSnapshot {
        self.snapshot
    }

    /// Reads current facts for delayed publication, without re-running the restore.
    /// Run on a blocking worker; the page mutation stays reserved until adoption.
    pub(crate) fn refresh_snapshot(mut self) -> Result<Self, String> {
        let (snapshot, observation) = self
            .manager
            .read_observed_snapshot()
            .map_err(|error| error.to_string())?
            .into_parts();
        self.snapshot = snapshot;
        self.preferences_observation = observation;
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn committed_for_test(manager: BackupManager, runtime_generation: u64) -> Self {
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

#[derive(Debug)]
pub(crate) struct BackupRecoveryReceipt {
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
    pub(crate) fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }
    pub(crate) fn into_observed_snapshot(
        self,
    ) -> Result<zenclash_core::ObservedBackupSnapshot, String> {
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
