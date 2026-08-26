use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Component, Path},
};

use zip::ZipArchive;

use super::super::{
    transaction::create_unique_directory, BackupError, BackupManager, BackupResult,
    PreparedBackupRestore, BACKUP_FORMAT_VERSION, CONTROLLED_PATH, MANIFEST_PATH,
    MAX_ARCHIVE_BYTES, MAX_BACKUP_BYTES, MAX_BACKUP_FILES, PREFERENCES_PATH, PROFILE_INDEX_PATH,
    YAML_OVERRIDE_INDEX_PATH,
};
use super::{
    ensure_payload_limits, is_authoritative_path, sha256_hex, validate_catalog_metadata,
    BackupManifest, ManifestFile,
};
use crate::{
    profiles::read_profile_bytes, validate_clash_yaml, AppPreferencesStore, ControlledConfigStore,
    ProfileCatalog, ProfileStoreError, YamlOverrideCatalog, YamlOverrideStore,
};

const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

pub(in crate::backup) fn prepare_restore(
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
        if !seen.insert(normalized.clone()) {
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
        let bytes = read_limited(&mut entry, MAX_BACKUP_BYTES, &normalized)?;
        payload_bytes = payload_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| BackupError::TooLarge("未压缩数据大小溢出".into()))?;
        ensure_payload_limits(observed.len() + 1, payload_bytes)?;
        let destination = staging_root.join(&normalized);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, &bytes)?;
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

fn validate_entry_type(entry: &zip::read::ZipFile<'_>) -> BackupResult<()> {
    if entry.encrypted() {
        return Err(BackupError::InvalidArchive(format!(
            "不支持加密条目 {}",
            entry.name()
        )));
    }
    if entry.is_dir() || entry.is_symlink() {
        return Err(BackupError::InvalidArchive(format!(
            "条目必须是普通文件：{}",
            entry.name()
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
    for file in &manifest.files {
        if !is_authoritative_path(&file.path) {
            return Err(BackupError::InvalidArchive(format!(
                "清单包含不允许的路径 {}",
                file.path
            )));
        }
        if declared.insert(file.path.clone(), file.clone()).is_some() {
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
    let index = fs::read(staging_root.join(PROFILE_INDEX_PATH))?;
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

fn normalized_archive_path(entry: &zip::read::ZipFile<'_>) -> BackupResult<String> {
    let enclosed = entry.enclosed_name().ok_or_else(|| {
        BackupError::InvalidArchive(format!("不安全的 ZIP 路径 {}", entry.name()))
    })?;
    let normalized = enclosed
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| BackupError::InvalidArchive("ZIP 路径不是 UTF-8".into())),
            _ => Err(BackupError::InvalidArchive(format!(
                "不安全的 ZIP 路径 {}",
                entry.name()
            ))),
        })
        .collect::<BackupResult<Vec<_>>>()?
        .join("/");
    if normalized != entry.name() {
        return Err(BackupError::InvalidArchive(format!(
            "ZIP 路径未规范化：{}",
            entry.name()
        )));
    }
    Ok(normalized)
}

fn read_limited(reader: &mut impl Read, limit: u64, label: &str) -> BackupResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(BackupError::TooLarge(format!("{label} 超过大小限制")));
    }
    Ok(bytes)
}
