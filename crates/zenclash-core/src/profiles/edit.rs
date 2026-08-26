use super::{
    atomic_write, read_profile_bytes, validate_clash_yaml, ProfileStore, ProfileStoreError,
    ProfileStoreResult, ProfileUpdate, MAX_PROFILE_BYTES,
};

impl ProfileStore {
    /// Atomically replaces a managed profile after checking the editor's base payload.
    ///
    /// The returned token can restore both the profile metadata and file when
    /// a subsequent Mihomo reload rejects the candidate.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile changed since it was opened, the new
    /// payload is invalid or oversized, or either file/index transaction fails.
    pub fn replace_payload(
        &self,
        id: &str,
        expected_payload: &str,
        new_payload: &str,
    ) -> ProfileStoreResult<ProfileUpdate> {
        if new_payload.len() > MAX_PROFILE_BYTES {
            return Err(ProfileStoreError::InvalidYaml(format!(
                "配置文件超过 {} MiB 限制",
                MAX_PROFILE_BYTES / 1024 / 1024
            )));
        }
        validate_clash_yaml(new_payload)?;

        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let index = catalog
            .profiles
            .iter()
            .position(|profile| profile.id == id)
            .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
        let path = self.profile_path(&catalog.profiles[index]);
        let previous_payload = read_profile_bytes(&path)?;
        if previous_payload != expected_payload.as_bytes() {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {id} 在编辑期间已改变，请重新打开后再保存"
            )));
        }

        let previous_record = catalog.profiles[index].clone();
        let applied_payload = new_payload.as_bytes().to_vec();
        atomic_write(&path, &applied_payload)?;
        catalog.profiles[index].updated_at = super::unix_timestamp();
        catalog.profiles[index].size_bytes = applied_payload.len() as u64;
        let record = catalog.profiles[index].clone();
        if let Err(error) = self.save_unlocked(&catalog) {
            return match atomic_write(&path, &previous_payload) {
                Ok(()) => Err(error),
                Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                    "保存配置索引失败：{error}；恢复编辑前文件失败：{rollback}"
                ))),
            };
        }
        Ok(ProfileUpdate {
            record,
            previous_record,
            previous_payload,
            applied_payload,
        })
    }
}
