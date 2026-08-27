use std::{
    io::{Cursor, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::super::{
    BACKUP_FORMAT_VERSION, BackupError, BackupExportSummary, BackupManager, BackupResult,
    CONTROLLED_PATH, MANIFEST_PATH, MAX_ARCHIVE_BYTES, PREFERENCES_PATH, PROFILE_INDEX_PATH,
    YAML_OVERRIDE_INDEX_PATH,
};
use super::{
    BackupManifest, ManifestFile, SnapshotFile, ensure_payload_limits, sha256_hex,
    validate_catalog_metadata,
};
use crate::{
    AppPreferencesStore, ControlledConfigStore, ProfileStore, ProfileStoreError, YamlOverrideStore,
    profiles::{atomic_write, read_profile_bytes},
    validate_clash_yaml,
};

pub(in crate::backup) fn export(
    manager: &BackupManager,
    destination: &Path,
) -> BackupResult<BackupExportSummary> {
    let files = collect_snapshot(manager)?;
    let payload_bytes = files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.bytes.len() as u64)
            .ok_or_else(|| BackupError::TooLarge("未压缩数据大小溢出".into()))
    })?;
    ensure_payload_limits(files.len(), payload_bytes)?;
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: unix_timestamp(),
        files: files
            .iter()
            .map(|file| ManifestFile {
                path: file.path.clone(),
                size: file.bytes.len() as u64,
                sha256: sha256_hex(&file.bytes),
            })
            .collect(),
    };
    let manifest = serde_json::to_vec_pretty(&manifest)?;
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    zip.start_file(MANIFEST_PATH, options)?;
    zip.write_all(&manifest)?;
    for file in &files {
        zip.start_file(&file.path, options)?;
        zip.write_all(&file.bytes)?;
    }
    let archive = zip.finish()?.into_inner();
    if archive.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(BackupError::TooLarge(format!(
            "压缩归档超过 {} MiB",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        )));
    }
    atomic_write(destination, &archive)?;
    Ok(BackupExportSummary {
        path: destination.to_path_buf(),
        file_count: files.len(),
        payload_bytes,
    })
}

fn collect_snapshot(manager: &BackupManager) -> BackupResult<Vec<SnapshotFile>> {
    let preferences =
        AppPreferencesStore::new(manager.data_root().join(PREFERENCES_PATH)).load()?;
    let controlled =
        ControlledConfigStore::new(manager.data_root().join("controlled-config")).load()?;
    let profiles = ProfileStore::new(manager.data_root().join("profiles"))?;
    let catalog = profiles.load()?;
    validate_catalog_metadata(&catalog)?;
    let overrides = YamlOverrideStore::new(manager.data_root().join("yaml-overrides"))?;
    let override_catalog = overrides.load()?;

    let mut files = vec![
        SnapshotFile {
            path: PREFERENCES_PATH.into(),
            bytes: serde_json::to_vec_pretty(&preferences)?,
        },
        SnapshotFile {
            path: CONTROLLED_PATH.into(),
            bytes: serde_yaml::to_string(&controlled)
                .map_err(crate::ControlledConfigError::from)?
                .into_bytes(),
        },
        SnapshotFile {
            path: PROFILE_INDEX_PATH.into(),
            bytes: serde_json::to_vec_pretty(&catalog)?,
        },
        SnapshotFile {
            path: YAML_OVERRIDE_INDEX_PATH.into(),
            bytes: serde_json::to_vec_pretty(&override_catalog)?,
        },
    ];
    for profile in &catalog.profiles {
        let bytes = read_profile_bytes(&profiles.profile_path(profile))?;
        let payload = std::str::from_utf8(&bytes).map_err(|error| {
            BackupError::Profiles(ProfileStoreError::InvalidYaml(format!(
                "配置 {} 不是 UTF-8：{error}",
                profile.file_name
            )))
        })?;
        validate_clash_yaml(payload)?;
        files.push(SnapshotFile {
            path: format!("profiles/files/{}", profile.file_name),
            bytes,
        });
    }
    for record in &override_catalog.items {
        files.push(SnapshotFile {
            path: format!("yaml-overrides/files/{}", record.file_name),
            bytes: std::fs::read(overrides.managed_path(record))?,
        });
    }
    Ok(files)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
