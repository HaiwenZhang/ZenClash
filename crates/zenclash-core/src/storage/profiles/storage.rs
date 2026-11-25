use std::path::Path;

pub use crate::platform::fs_io::atomic_write;
use crate::platform::fs_io::{
    BoundedReadError, MAX_PROFILE_BYTES, read_bounded_profile_bytes, read_bounded_regular_file,
};

use super::{
    MAX_PROFILE_INDEX_BYTES, ProfileCatalog, ProfileStore, ProfileStoreError, ProfileStoreResult,
};

impl ProfileStore {
    /// Saves an updated catalog after its payload write, restoring the payload on failure.
    /// The caller retains the transaction lock and owns revision checks and write ordering.
    /// New-profile cleanup and active-selection recovery have different semantics and
    /// remain in their existing transaction paths.
    pub(crate) fn save_catalog_or_restore_payload_unlocked(
        &self,
        catalog: &ProfileCatalog,
        path: &Path,
        previous_payload: &[u8],
        catalog_failure: &str,
        restore_failure: &str,
    ) -> ProfileStoreResult<()> {
        let Err(error) = self.save_unlocked(catalog) else {
            return Ok(());
        };
        match atomic_write(path, previous_payload) {
            Ok(()) => Err(error),
            Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                "{catalog_failure}：{error}；{restore_failure}：{rollback}"
            ))),
        }
    }
}

pub fn read_profile_bytes(path: &Path) -> ProfileStoreResult<Vec<u8>> {
    read_bounded_profile_bytes(path).map_err(|error| match error {
        BoundedReadError::Io(io_error) => ProfileStoreError::Io(io_error),
        BoundedReadError::TooLarge { .. } => ProfileStoreError::InvalidYaml(format!(
            "配置文件超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )),
    })
}

pub(crate) fn read_index_bytes(path: &Path) -> ProfileStoreResult<Vec<u8>> {
    read_bounded_regular_file(path, MAX_PROFILE_INDEX_BYTES).map_err(|error| match error {
        BoundedReadError::Io(io_error) => ProfileStoreError::Io(io_error),
        BoundedReadError::TooLarge { .. } => ProfileStoreError::IndexTooLarge {
            limit_mib: MAX_PROFILE_INDEX_BYTES / 1024 / 1024,
        },
    })
}

pub(crate) fn encode_catalog(catalog: &super::ProfileCatalog) -> ProfileStoreResult<Vec<u8>> {
    super::validate_catalog_records(catalog)?;
    let bytes = serde_json::to_vec_pretty(catalog)?;
    if bytes.len() > MAX_PROFILE_INDEX_BYTES {
        return Err(ProfileStoreError::IndexTooLarge {
            limit_mib: MAX_PROFILE_INDEX_BYTES / 1024 / 1024,
        });
    }
    Ok(bytes)
}
