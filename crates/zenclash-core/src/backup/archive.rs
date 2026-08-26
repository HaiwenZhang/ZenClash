use std::{
    collections::HashSet,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    BackupError, BackupResult, CONTROLLED_PATH, MAX_BACKUP_BYTES, MAX_BACKUP_FILES,
    PREFERENCES_PATH, PROFILE_INDEX_PATH, YAML_OVERRIDE_INDEX_PATH,
};
use crate::ProfileCatalog;

mod export;
mod import;

pub(super) use export::export;
pub(super) use import::prepare_restore;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    format_version: u32,
    app_version: String,
    created_at: u64,
    files: Vec<ManifestFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    size: u64,
    sha256: String,
}

struct SnapshotFile {
    path: String,
    bytes: Vec<u8>,
}

fn validate_catalog_metadata(catalog: &ProfileCatalog) -> BackupResult<()> {
    let mut ids = HashSet::new();
    let mut file_names = HashSet::new();
    for profile in &catalog.profiles {
        if profile.id.trim().is_empty() || !ids.insert(profile.id.as_str()) {
            return Err(BackupError::InvalidArchive(
                "配置索引包含空白或重复 ID".into(),
            ));
        }
        if !safe_profile_file_name(&profile.file_name)
            || !file_names.insert(profile.file_name.as_str())
        {
            return Err(BackupError::InvalidArchive(format!(
                "配置索引包含不安全或重复文件名 {}",
                profile.file_name
            )));
        }
    }
    if catalog
        .active
        .as_ref()
        .is_some_and(|active| !catalog.profiles.iter().any(|profile| &profile.id == active))
    {
        return Err(BackupError::InvalidArchive(
            "活动配置 ID 不存在于配置索引".into(),
        ));
    }
    Ok(())
}

fn safe_profile_file_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && Path::new(name).extension().and_then(|value| value.to_str()) == Some("yaml")
}

fn is_authoritative_path(path: &str) -> bool {
    matches!(
        path,
        PREFERENCES_PATH | CONTROLLED_PATH | PROFILE_INDEX_PATH | YAML_OVERRIDE_INDEX_PATH
    ) || path
        .strip_prefix("profiles/files/")
        .is_some_and(safe_profile_file_name)
        || path
            .strip_prefix("yaml-overrides/files/")
            .is_some_and(safe_profile_file_name)
}

fn ensure_payload_limits(file_count: usize, payload_bytes: u64) -> BackupResult<()> {
    if file_count > MAX_BACKUP_FILES {
        return Err(BackupError::TooLarge(format!(
            "文件数超过 {MAX_BACKUP_FILES}"
        )));
    }
    if payload_bytes > MAX_BACKUP_BYTES {
        return Err(BackupError::TooLarge(format!(
            "未压缩数据超过 {} MiB",
            MAX_BACKUP_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    Sha256::digest(bytes).iter().fold(
        String::with_capacity(Sha256::output_size() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
}
