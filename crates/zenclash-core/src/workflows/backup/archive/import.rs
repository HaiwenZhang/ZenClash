use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Component, Path},
};

use zip::ZipArchive;

use super::super::{
    BACKUP_FORMAT_VERSION, BackupError, BackupManager, BackupResult, CONTROLLED_PATH,
    MANIFEST_PATH, MAX_ARCHIVE_BYTES, MAX_BACKUP_BYTES, MAX_BACKUP_FILES, PREFERENCES_PATH,
    PROFILE_INDEX_PATH, PreparedBackupRestore, YAML_OVERRIDE_INDEX_PATH,
    transaction::create_unique_directory,
};
use super::{
    BackupManifest, ManifestFile, ensure_payload_limits, is_authoritative_path, sha256_hex,
    validate_catalog_metadata,
};
use crate::{
    AppPreferencesStore, ControlledConfigStore, ProfileCatalog, ProfileStoreError,
    YamlOverrideCatalog, YamlOverrideStore, storage::profiles::read_profile_bytes,
    validate_clash_yaml,
};

const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PREFERENCES_ENTRY_BYTES: u64 = 1024 * 1024;
const MAX_PROFILE_INDEX_ENTRY_BYTES: u64 = 4 * 1024 * 1024;

pub(in crate::workflows::backup) fn prepare_restore(
    manager: &BackupManager,
    archive_path: &Path,
) -> BackupResult<PreparedBackupRestore> {
    let archive_size = fs::metadata(archive_path)?.len();
    if archive_size > MAX_ARCHIVE_BYTES {
        return Err(BackupError::TooLarge(format!(
            "ZIP 为 {} MiB，限制为 {} MiB",
            archive_size / 1024 / 1024,
            MAX_ARCHIVE_BYTES / 1024 / 1024
        )));
    }
    let parent = manager
        .data_root()
        .parent()
        .ok_or(BackupError::MissingDataDirectory)?;
    fs::create_dir_all(parent)?;
    let staging_root = create_unique_directory(parent, ".zenclash-backup-staging")?;
    let result = extract_and_validate(archive_path, &staging_root);
    match result {
        Ok((file_count, payload_bytes)) => Ok(PreparedBackupRestore {
            data_root: manager.data_root().to_path_buf(),
            staging_root,
            file_count,
            payload_bytes,
            previous_runtime: None,
        }),
        Err(error) => {
            if let Err(cleanup) = fs::remove_dir_all(&staging_root) {
                return Err(BackupError::Transaction(format!(
                    "导入失败：{error}；清理暂存目录失败：{cleanup}"
                )));
            }
            Err(error)
        }
    }
}

fn extract_and_validate(archive_path: &Path, staging_root: &Path) -> BackupResult<(usize, u64)> {
    extract_and_validate_with_budget(archive_path, staging_root, MAX_BACKUP_BYTES)
}

fn extract_and_validate_with_budget(
    archive_path: &Path,
    staging_root: &Path,
    max_backup_bytes: u64,
) -> BackupResult<(usize, u64)> {
    let mut zip = ZipArchive::new(fs::File::open(archive_path)?)?;
    if zip.len() > MAX_BACKUP_FILES + 1 {
        return Err(BackupError::TooLarge(format!(
            "ZIP 条目数超过 {}",
            MAX_BACKUP_FILES + 1
        )));
    }
    let mut manifest = None;
    let mut observed = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut payload_bytes = 0_u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        validate_entry_type(&entry)?;
        let normalized = normalized_archive_path(&entry)?;
        if !seen.insert(normalized.to_lowercase()) {
            return Err(BackupError::InvalidArchive(format!(
                "ZIP 包含重复条目 {normalized}"
            )));
        }
        if normalized == MANIFEST_PATH {
            let bytes = read_limited(&mut entry, MAX_MANIFEST_BYTES, "清单")?;
            manifest = Some(serde_json::from_slice::<BackupManifest>(&bytes)?);
            continue;
        }
        if !is_authoritative_path(&normalized) {
            return Err(BackupError::InvalidArchive(format!(
                "不允许恢复路径 {normalized}"
            )));
        }
        let remaining_budget = max_backup_bytes.saturating_sub(payload_bytes);
        let per_type = entry_byte_limit(&normalized);
        let entry_limit = per_type.map_or(remaining_budget, |limit| limit.min(remaining_budget));
        let bytes = read_limited(&mut entry, entry_limit, &normalized).map_err(|error| {
            if normalized == PROFILE_INDEX_PATH
                && matches!(error, BackupError::TooLarge(_))
                && remaining_budget >= MAX_PROFILE_INDEX_ENTRY_BYTES
            {
                BackupError::Profiles(ProfileStoreError::IndexTooLarge {
                    limit_mib: (MAX_PROFILE_INDEX_ENTRY_BYTES / (1024 * 1024)) as usize,
                })
            } else {
                error
            }
        })?;
        payload_bytes = payload_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| BackupError::TooLarge("未压缩数据大小溢出".into()))?;
        if payload_bytes > max_backup_bytes {
            return Err(BackupError::TooLarge(format!(
                "未压缩数据超过 {max_backup_bytes} 字节"
            )));
        }
        ensure_payload_limits(observed.len() + 1, payload_bytes)?;
        let destination = staging_root.join(&normalized);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::platform::fs_io::atomic_write(&destination, &bytes)?;
        observed.insert(
            normalized,
            ManifestFile {
                path: String::new(),
                size: bytes.len() as u64,
                sha256: sha256_hex(&bytes),
            },
        );
    }
    let manifest = manifest
        .ok_or_else(|| BackupError::InvalidArchive(format!("缺少必需的 {MANIFEST_PATH}")))?;
    verify_manifest(&manifest, &observed)?;
    if manifest.format_version == 1 {
        YamlOverrideStore::new(staging_root.join("yaml-overrides"))?;
    }
    validate_staged_snapshot(staging_root)?;
    Ok((observed.len(), payload_bytes))
}

fn entry_byte_limit(path: &str) -> Option<u64> {
    match path {
        PREFERENCES_PATH => Some(MAX_PREFERENCES_ENTRY_BYTES),
        CONTROLLED_PATH => Some(crate::platform::fs_io::MAX_PROFILE_BYTES as u64),
        PROFILE_INDEX_PATH => Some(MAX_PROFILE_INDEX_ENTRY_BYTES),
        YAML_OVERRIDE_INDEX_PATH => Some(crate::storage::overrides::MAX_MANIFEST_BYTES as u64),
        _ if path.starts_with("profiles/files/") => {
            Some(crate::platform::fs_io::MAX_PROFILE_BYTES as u64)
        }
        _ if path.starts_with("yaml-overrides/files/") => {
            Some(crate::storage::overrides::MAX_OVERRIDE_BYTES as u64)
        }
        _ => None,
    }
}

fn validate_entry_type<R: Read + ?Sized>(entry: &zip::read::ZipFile<'_, R>) -> BackupResult<()> {
    let name = entry.name()?;
    if entry.encrypted() {
        return Err(BackupError::InvalidArchive(format!(
            "不支持加密条目 {name}"
        )));
    }
    if entry.is_dir() || entry.is_symlink() {
        return Err(BackupError::InvalidArchive(format!(
            "条目必须是普通文件：{name}"
        )));
    }
    Ok(())
}

fn verify_manifest(
    manifest: &BackupManifest,
    observed: &BTreeMap<String, ManifestFile>,
) -> BackupResult<()> {
    if !matches!(manifest.format_version, 1 | BACKUP_FORMAT_VERSION) {
        return Err(BackupError::InvalidArchive(format!(
            "不支持格式版本 {}，当前支持 1 和 {}",
            manifest.format_version, BACKUP_FORMAT_VERSION
        )));
    }
    let mut declared = BTreeMap::new();
    let mut declared_folded = HashSet::new();
    for file in &manifest.files {
        if !is_authoritative_path(&file.path) {
            return Err(BackupError::InvalidArchive(format!(
                "清单包含不允许的路径 {}",
                file.path
            )));
        }
        if !declared_folded.insert(file.path.to_lowercase())
            || declared.insert(file.path.clone(), file.clone()).is_some()
        {
            return Err(BackupError::InvalidArchive(format!(
                "清单包含重复路径 {}",
                file.path
            )));
        }
    }
    for required in [PREFERENCES_PATH, CONTROLLED_PATH, PROFILE_INDEX_PATH] {
        if !declared.contains_key(required) {
            return Err(BackupError::InvalidArchive(format!(
                "清单缺少必需文件 {required}"
            )));
        }
    }
    if manifest.format_version >= 2 && !declared.contains_key(YAML_OVERRIDE_INDEX_PATH) {
        return Err(BackupError::InvalidArchive(format!(
            "清单缺少必需文件 {YAML_OVERRIDE_INDEX_PATH}"
        )));
    }
    if declared.len() != observed.len() {
        return Err(BackupError::InvalidArchive(format!(
            "清单声明 {} 个文件，但 ZIP 包含 {} 个数据文件",
            declared.len(),
            observed.len()
        )));
    }
    for (path, expected) in declared {
        let actual = observed
            .get(&path)
            .ok_or_else(|| BackupError::InvalidArchive(format!("清单文件 {path} 不存在于 ZIP")))?;
        if expected.size != actual.size || expected.sha256 != actual.sha256 {
            return Err(BackupError::InvalidArchive(format!(
                "文件 {path} 的大小或 SHA-256 与清单不一致"
            )));
        }
    }
    Ok(())
}

fn validate_staged_snapshot(staging_root: &Path) -> BackupResult<()> {
    AppPreferencesStore::new(staging_root.join(PREFERENCES_PATH)).load()?;
    ControlledConfigStore::new(staging_root.join("controlled-config")).load()?;
    let index = crate::storage::profiles::read_index_bytes(&staging_root.join(PROFILE_INDEX_PATH))?;
    let catalog: ProfileCatalog = serde_json::from_slice(&index)?;
    validate_catalog_metadata(&catalog)?;
    let overrides = YamlOverrideStore::new(staging_root.join("yaml-overrides"))?;
    let override_catalog = overrides.load()?;
    for profile in &catalog.profiles {
        let path = staging_root.join("profiles/files").join(&profile.file_name);
        let bytes = read_profile_bytes(&path)?;
        if profile.size_bytes != bytes.len() as u64 {
            return Err(BackupError::InvalidArchive(format!(
                "配置 {} 的索引大小与实际文件不一致",
                profile.file_name
            )));
        }
        let payload = std::str::from_utf8(&bytes).map_err(|error| {
            BackupError::Profiles(ProfileStoreError::InvalidYaml(format!(
                "配置 {} 不是 UTF-8：{error}",
                profile.file_name
            )))
        })?;
        validate_clash_yaml(payload)?;
    }
    validate_profile_file_set(staging_root, &catalog)?;
    validate_override_file_set(staging_root, &override_catalog)
}

fn validate_override_file_set(
    staging_root: &Path,
    catalog: &YamlOverrideCatalog,
) -> BackupResult<()> {
    let expected = catalog
        .items
        .iter()
        .map(|record| format!("yaml-overrides/files/{}", record.file_name))
        .collect::<HashSet<_>>();
    let override_files = staging_root.join("yaml-overrides/files");
    let actual = if override_files.exists() {
        fs::read_dir(override_files)?
            .map(|entry| {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    return Err(BackupError::InvalidArchive(format!(
                        "YAML 覆写目录包含非普通文件 {}",
                        entry.path().display()
                    )));
                }
                Ok(format!(
                    "yaml-overrides/files/{}",
                    entry.file_name().to_string_lossy()
                ))
            })
            .collect::<BackupResult<HashSet<_>>>()?
    } else {
        HashSet::new()
    };
    if expected != actual {
        return Err(BackupError::InvalidArchive(
            "YAML 覆写清单与 yaml-overrides/files 中的文件集合不一致".into(),
        ));
    }
    Ok(())
}

fn validate_profile_file_set(staging_root: &Path, catalog: &ProfileCatalog) -> BackupResult<()> {
    let expected = catalog
        .profiles
        .iter()
        .map(|profile| format!("profiles/files/{}", profile.file_name))
        .collect::<HashSet<_>>();
    let profile_files = staging_root.join("profiles/files");
    let actual = if profile_files.exists() {
        fs::read_dir(profile_files)?
            .map(|entry| {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    return Err(BackupError::InvalidArchive(format!(
                        "配置目录包含非普通文件 {}",
                        entry.path().display()
                    )));
                }
                Ok(format!(
                    "profiles/files/{}",
                    entry.file_name().to_string_lossy()
                ))
            })
            .collect::<BackupResult<HashSet<_>>>()?
    } else {
        HashSet::new()
    };
    if expected != actual {
        return Err(BackupError::InvalidArchive(
            "配置索引与 profiles/files 中的 YAML 集合不一致".into(),
        ));
    }
    Ok(())
}

fn normalized_archive_path<R: Read + ?Sized>(
    entry: &zip::read::ZipFile<'_, R>,
) -> BackupResult<String> {
    let name = entry.name()?;
    let enclosed = entry
        .enclosed_name()
        .ok_or_else(|| BackupError::InvalidArchive(format!("不安全的 ZIP 路径 {name}")))?;
    let normalized = enclosed
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| BackupError::InvalidArchive("ZIP 路径不是 UTF-8".into())),
            _ => Err(BackupError::InvalidArchive(format!(
                "不安全的 ZIP 路径 {name}"
            ))),
        })
        .collect::<BackupResult<Vec<_>>>()?
        .join("/");
    if normalized != name.as_ref() {
        return Err(BackupError::InvalidArchive(format!(
            "ZIP 路径未规范化：{name}"
        )));
    }
    Ok(normalized)
}

fn read_limited(reader: &mut impl Read, limit: u64, label: &str) -> BackupResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(BackupError::TooLarge(format!("{label} 超过大小限制")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write as _};

    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::*;

    #[test]
    fn extract_deducts_remaining_total_budget_across_entries() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-backup-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let archive = root.join("budget.zip");
        let staging = root.join("staging");
        fs::create_dir_all(&staging).unwrap();

        let first = vec![b'a'; 600];
        let second = vec![b'b'; 600];
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file("profiles/files/first.yaml", options)
            .unwrap();
        writer.write_all(&first).unwrap();
        writer
            .start_file("profiles/files/second.yaml", options)
            .unwrap();
        writer.write_all(&second).unwrap();
        fs::write(&archive, writer.finish().unwrap().into_inner()).unwrap();

        // With a 1,000-byte total budget, first.yaml uses 600 bytes leaving only 400 bytes,
        // so reading second.yaml (600 bytes) must fail immediately during streaming read
        // before writing second.yaml to staging.
        let error = extract_and_validate_with_budget(&archive, &staging, 1_000).unwrap_err();
        assert!(matches!(error, BackupError::TooLarge(_)), "{error}");
        assert!(staging.join("profiles/files/first.yaml").is_file());
        assert!(!staging.join("profiles/files/second.yaml").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verify_manifest_rejects_case_insensitive_duplicate_paths() {
        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            app_version: "0.3.0".into(),
            created_at: 0,
            files: vec![
                ManifestFile {
                    path: "yaml-overrides/files/a.yaml".into(),
                    size: 1,
                    sha256: "00".into(),
                },
                ManifestFile {
                    path: "yaml-overrides/files/A.yaml".into(),
                    size: 1,
                    sha256: "11".into(),
                },
            ],
        };
        let error = verify_manifest(&manifest, &BTreeMap::new()).unwrap_err();
        assert!(matches!(error, BackupError::InvalidArchive(_)), "{error}");
    }
}
