use std::{fs, path::PathBuf};

use super::{ProfileActivation, ProfileStore, ProfileStoreError, ProfileStoreResult};

impl ProfileStore {
    /// Persists a new active profile and returns its managed YAML path.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile does not exist or the catalog cannot
    /// be persisted atomically.
    pub fn activate(&self, id: &str) -> ProfileStoreResult<PathBuf> {
        Ok(self.activate_reversible(id)?.path)
    }

    /// Persists a new active profile and returns a token that can restore the
    /// previous catalog state if Mihomo rejects the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile does not exist or the catalog cannot
    /// be persisted atomically.
    pub fn activate_reversible(&self, id: &str) -> ProfileStoreResult<ProfileActivation> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let profile = catalog
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
        let path = self.profile_path(profile);
        let previous_active = catalog.active.clone();
        catalog.active = Some(id.into());
        self.save_unlocked(&catalog)?;
        Ok(ProfileActivation {
            activated_id: id.into(),
            previous_active,
            path,
        })
    }

    /// Restores the active profile captured by [`Self::activate_reversible`].
    ///
    /// The rollback refuses to overwrite a newer activation performed after
    /// the token was created.
    ///
    /// # Errors
    ///
    /// Returns an error when the active profile has changed or the previous
    /// catalog state cannot be persisted.
    pub fn rollback_activation(&self, activation: ProfileActivation) -> ProfileStoreResult<()> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        if catalog.active.as_deref() != Some(activation.activated_id.as_str()) {
            return Err(ProfileStoreError::Transaction(format!(
                "活动配置已从 {} 变更，拒绝覆盖较新的选择",
                activation.activated_id
            )));
        }
        catalog.active = activation.previous_active;
        self.save_unlocked(&catalog)
    }

    /// Deletes a non-active profile from the catalog and disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is active or missing, or when the
    /// file/index transaction cannot be completed.
    pub fn delete(&self, id: &str) -> ProfileStoreResult<()> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        if catalog.active.as_deref() == Some(id) {
            return Err(ProfileStoreError::ActiveProfile);
        }
        let index = catalog
            .profiles
            .iter()
            .position(|profile| profile.id == id)
            .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
        let previous_catalog = catalog.clone();
        let profile = catalog.profiles.remove(index);
        let path = self.profile_path(&profile);
        self.save_unlocked(&catalog)?;
        if path.exists() {
            if let Err(error) = fs::remove_file(&path) {
                return match self.save_unlocked(&previous_catalog) {
                    Ok(()) => Err(error.into()),
                    Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                        "删除配置文件失败：{error}；恢复配置索引失败：{rollback}"
                    ))),
                };
            }
        }
        Ok(())
    }
}
