use std::{
    fs::OpenOptions,
    io::{Cursor, Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::super::{
    BACKUP_FORMAT_VERSION, BackupError, BackupExportSummary, BackupManager, BackupResult,
    CONTROLLED_PATH, MANIFEST_PATH, MAX_ARCHIVE_BYTES, MAX_BACKUP_BYTES, PREFERENCES_PATH,
    PROFILE_INDEX_PATH, YAML_OVERRIDE_INDEX_PATH,
};
use super::{
    BackupManifest, ManifestFile, SnapshotFile, ensure_payload_limits, sha256_hex,
    validate_catalog_metadata,
};
use crate::{
    AppPreferencesStore, ControlledConfigStore, ProfileStore, ProfileStoreError, YamlOverrideStore,
    profiles::{MAX_PROFILE_BYTES, atomic_write},
    validate_clash_yaml,
};

pub(in crate::backup) fn export(
    manager: &BackupManager,
    destination: &Path,
) -> BackupResult<BackupExportSummary> {
    let mut scopes = vec![manager.data_root().to_path_buf(), destination.to_path_buf()];
    scopes.extend(super::super::transaction::write_scopes(manager.data_root()));
    // Catalogs and their payloads form one snapshot. Ordinary writes also hold
    // shared leases, so a shared root reservation would not freeze this read.
    let lease = crate::data_coordinator::DataWriteLease::exclusive(scopes);
    super::super::ensure_writes_available(lease.scopes())?;
    let BackupSnapshot {
        files,
        payload_bytes,
    } = collect_snapshot(manager, &lease)?;
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

fn collect_snapshot(
    manager: &BackupManager,
    lease: &crate::data_coordinator::DataWriteLease,
) -> BackupResult<BackupSnapshot> {
    let preferences =
        AppPreferencesStore::new(manager.data_root().join(PREFERENCES_PATH)).load()?;
    let controlled =
        ControlledConfigStore::new(manager.data_root().join("controlled-config")).load()?;
    let profiles = ProfileStore::open(manager.data_root().join("profiles"), Some(lease))?;
    let catalog = profiles.load()?;
    validate_catalog_metadata(&catalog)?;
    let overrides =
        YamlOverrideStore::open(manager.data_root().join("yaml-overrides"), Some(lease))?;
    let override_catalog = overrides.load_catalog_metadata()?;

    let mut snapshot = BackupSnapshot::default();
    snapshot.push(
        PREFERENCES_PATH.into(),
        serde_json::to_vec_pretty(&preferences)?,
    )?;
    snapshot.push(
        CONTROLLED_PATH.into(),
        serde_yaml::to_string(&controlled)
            .map_err(crate::ControlledConfigError::from)?
            .into_bytes(),
    )?;
    snapshot.push(
        PROFILE_INDEX_PATH.into(),
        crate::profiles::encode_catalog(&catalog)?,
    )?;
    snapshot.push(
        YAML_OVERRIDE_INDEX_PATH.into(),
        serde_json::to_vec_pretty(&override_catalog)?,
    )?;
    ensure_payload_limits(
        snapshot.files.len() + catalog.profiles.len() + override_catalog.items.len(),
        snapshot.payload_bytes,
    )?;
    for profile in &catalog.profiles {
        let bytes =
            snapshot.read_bounded_file(&profiles.profile_path(profile), MAX_PROFILE_BYTES)?;
        let payload = std::str::from_utf8(&bytes).map_err(|error| {
            BackupError::Profiles(ProfileStoreError::InvalidYaml(format!(
                "配置 {} 不是 UTF-8：{error}",
                profile.file_name
            )))
        })?;
        validate_clash_yaml(payload)?;
        if bytes.len() as u64 != profile.size_bytes {
            return Err(BackupError::InvalidArchive(format!(
                "配置 {} 的索引大小与实际内容不一致",
                profile.file_name
            )));
        }
        snapshot.push(format!("profiles/files/{}", profile.file_name), bytes)?;
    }
    for record in &override_catalog.items {
        let path = overrides.managed_path(record);
        let bytes = snapshot.read_bounded_file(&path, crate::yaml_overrides::MAX_OVERRIDE_BYTES)?;
        crate::yaml_overrides::validate_override_bytes(&path, &bytes)?;
        snapshot.push(format!("yaml-overrides/files/{}", record.file_name), bytes)?;
    }
    Ok(snapshot)
}

#[derive(Default)]
struct BackupSnapshot {
    files: Vec<SnapshotFile>,
    payload_bytes: u64,
}

impl BackupSnapshot {
    fn push(&mut self, path: String, bytes: Vec<u8>) -> BackupResult<()> {
        let payload_bytes = self
            .payload_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| BackupError::TooLarge("未压缩数据大小溢出".into()))?;
        ensure_payload_limits(self.files.len() + 1, payload_bytes)?;
        self.files.push(SnapshotFile { path, bytes });
        self.payload_bytes = payload_bytes;
        Ok(())
    }

    fn read_bounded_file(&self, path: &Path, file_limit: usize) -> BackupResult<Vec<u8>> {
        let limit = (file_limit as u64).min(MAX_BACKUP_BYTES.saturating_sub(self.payload_bytes));
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // An externally substituted FIFO must not block the snapshot owner.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(BackupError::InvalidArchive(format!(
                "备份数据不是普通文件：{}",
                path.display()
            )));
        }
        if metadata.len() > limit {
            return Err(BackupError::TooLarge(format!(
                "文件超过可用备份预算：{}",
                path.display()
            )));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(BackupError::TooLarge(format!(
                "文件超过可用备份预算：{}",
                path.display()
            )));
        }
        Ok(bytes)
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
