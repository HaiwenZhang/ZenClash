use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{BackupError, BackupRestoreTransaction, BackupResult, PreparedBackupRestore};

const LIVE_ITEMS: [&str; 4] = [
    "preferences.json",
    "controlled-config",
    "profiles",
    "yaml-overrides",
];
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn activate(
    mut prepared: PreparedBackupRestore,
) -> BackupResult<BackupRestoreTransaction> {
    let parent = prepared
        .data_root
        .parent()
        .ok_or(BackupError::MissingDataDirectory)?;
    let rollback_root = create_unique_directory(parent, ".zenclash-backup-rollback")?;
    let remove_empty_data_root = !prepared.data_root.exists();
    fs::create_dir_all(&prepared.data_root)?;
    let mut installed = Vec::new();
    let mut preserved = Vec::new();
    for item in LIVE_ITEMS {
        let staged = prepared.staging_root.join(item);
        let live = prepared.data_root.join(item);
        let previous = rollback_root.join(item);
        if live.exists() {
            fs::rename(&live, &previous).map_err(|error| {
                activation_failure(
                    &error,
                    &prepared.data_root,
                    &rollback_root,
                    &installed,
                    &preserved,
                    remove_empty_data_root,
                )
            })?;
            preserved.push(item);
        }
        if let Err(error) = fs::rename(&staged, &live) {
            return Err(activation_failure(
                &error,
                &prepared.data_root,
                &rollback_root,
                &installed,
                &preserved,
                remove_empty_data_root,
            ));
        }
        installed.push(item);
    }
    if let Err(error) = fs::remove_dir_all(&prepared.staging_root) {
        return Err(activation_failure(
            &error,
            &prepared.data_root,
            &rollback_root,
            &installed,
            &preserved,
            remove_empty_data_root,
        ));
    }
    prepared.staging_root = PathBuf::new();
    Ok(BackupRestoreTransaction {
        data_root: prepared.data_root.clone(),
        rollback_root,
        remove_empty_data_root,
        active: true,
    })
}

pub(super) fn rollback(transaction: &mut BackupRestoreTransaction) -> BackupResult<()> {
    restore_previous(
        &transaction.data_root,
        &transaction.rollback_root,
        &LIVE_ITEMS,
        &LIVE_ITEMS,
        transaction.remove_empty_data_root,
    )
}

pub(super) fn create_unique_directory(parent: &Path, prefix: &str) -> BackupResult<PathBuf> {
    for _ in 0..128 {
        let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("{prefix}-{}-{sequence}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(BackupError::Transaction(format!(
        "无法在 {} 创建唯一事务目录",
        parent.display()
    )))
}

fn activation_failure(
    error: &std::io::Error,
    data_root: &Path,
    rollback_root: &Path,
    installed: &[&str],
    preserved: &[&str],
    remove_empty_data_root: bool,
) -> BackupError {
    match restore_previous(
        data_root,
        rollback_root,
        installed,
        preserved,
        remove_empty_data_root,
    ) {
        Ok(()) => BackupError::Transaction(format!("激活导入数据失败，原数据已恢复：{error}")),
        Err(rollback) => BackupError::Transaction(format!(
            "激活导入数据失败：{error}；恢复原数据也失败：{rollback}"
        )),
    }
}

fn restore_previous(
    data_root: &Path,
    rollback_root: &Path,
    installed: &[&str],
    preserved: &[&str],
    remove_empty_data_root: bool,
) -> BackupResult<()> {
    for item in installed.iter().rev() {
        remove_path(&data_root.join(item))?;
    }
    for item in preserved.iter().rev() {
        let previous = rollback_root.join(item);
        if previous.exists() {
            fs::rename(previous, data_root.join(item))?;
        }
    }
    if rollback_root.exists() {
        fs::remove_dir_all(rollback_root)?;
    }
    if remove_empty_data_root && data_root.exists() && fs::read_dir(data_root)?.next().is_none() {
        fs::remove_dir(data_root)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> BackupResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
