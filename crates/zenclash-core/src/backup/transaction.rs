use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{BackupError, BackupRestoreTransaction, BackupResult, PreparedBackupRestore};

pub(super) const LIVE_ITEMS: [&str; 4] = [
    "preferences.json",
    "controlled-config",
    "profiles",
    "yaml-overrides",
];
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn write_scopes(root: &Path) -> Vec<PathBuf> {
    LIVE_ITEMS
        .into_iter()
        .chain(["profiles/files", "profiles/staging", "yaml-overrides/files"])
        .map(|item| root.join(item))
        .collect()
}

pub(super) fn activate(
    mut prepared: PreparedBackupRestore,
    session: Option<&crate::CoreSession>,
) -> BackupResult<BackupRestoreTransaction> {
    let mut runtime_scopes = session.map_or_else(Vec::new, crate::CoreSession::write_scopes);
    runtime_scopes.push(prepared.data_root.clone());
    // Rollback can restore an existing managed-directory symlink. Reserve its
    // current physical destination too, so subsequent runtime reconciliation
    // stays under the same exclusive authority.
    runtime_scopes.extend(write_scopes(&prepared.data_root));
    let lease = crate::data_coordinator::DataWriteLease::exclusive(runtime_scopes);
    let mut previous_runtime = session
        .map(|session| {
            let controlled =
                crate::ControlledConfigStore::new(prepared.data_root.join("controlled-config"))
                    .with_write_lease(&lease);
            session
                .capture_restore_snapshot(&controlled)
                .map_err(|error| BackupError::Transaction(error.to_string()))
        })
        .transpose()?;
    if let Some((expected, held)) = prepared.previous_runtime.take() {
        let current = session.ok_or_else(|| {
            BackupError::Transaction(zenclash_i18n::text("core_page.service.stale"))
        })?;
        if current.runtime_descriptor().backend() != crate::CoreRuntimeBackend::Local
            || (
                current.runtime_descriptor().binding_generation(),
                current.generation(),
            ) != expected
        {
            return Err(BackupError::Transaction(zenclash_i18n::text(
                "core_page.service.stale",
            )));
        }
        let actual = previous_runtime.as_ref().ok_or_else(|| {
            BackupError::Transaction(zenclash_i18n::text("core_page.service.no_snapshot"))
        })?;
        crate::CoreSession::validate_backup_snapshot(&held, actual)
            .map_err(|error| BackupError::Transaction(error.to_string()))?;
        previous_runtime = Some(held);
    }
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
        let exists = match fs::symlink_metadata(&live) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(activation_failure(
                    &error,
                    &prepared.data_root,
                    &rollback_root,
                    &mut installed,
                    &mut preserved,
                    remove_empty_data_root,
                ));
            }
        };
        if exists {
            fs::rename(&live, &previous).map_err(|error| {
                activation_failure(
                    &error,
                    &prepared.data_root,
                    &rollback_root,
                    &mut installed,
                    &mut preserved,
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
                &mut installed,
                &mut preserved,
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
            &mut installed,
            &mut preserved,
            remove_empty_data_root,
        ));
    }
    prepared.staging_root = PathBuf::new();
    Ok(BackupRestoreTransaction {
        data_root: prepared.data_root.clone(),
        rollback_root,
        remove_empty_data_root,
        installed,
        preserved,
        active: true,
        previous_runtime,
        lease,
    })
}

pub(super) fn rollback(transaction: &mut BackupRestoreTransaction) -> BackupResult<()> {
    restore_previous(
        &transaction.data_root,
        &transaction.rollback_root,
        &mut transaction.installed,
        &mut transaction.preserved,
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
    installed: &mut Vec<&'static str>,
    preserved: &mut Vec<&'static str>,
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
    installed: &mut Vec<&'static str>,
    preserved: &mut Vec<&'static str>,
    remove_empty_data_root: bool,
) -> BackupResult<()> {
    while let Some(item) = installed.last() {
        remove_path(&data_root.join(item))?;
        installed.pop();
    }
    // Record each successful move before proceeding. Drop may retry after a
    // later move or directory cleanup fails, and must preserve restored items.
    while let Some(item) = preserved.last() {
        fs::rename(rollback_root.join(item), data_root.join(item))?;
        preserved.pop();
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
