use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{BackupError, BackupRestoreTransaction, BackupResult, PreparedBackupRestore};

pub(super) const LIVE_ITEMS: [&str; 4] = [
    super::PREFERENCES_PATH,
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
    super::ensure_writes_available(lease.scopes())?;
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
            || current.runtime_revision() != expected
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
    let journal = super::recovery::RestoreJournal::create(
        &prepared.data_root,
        &rollback_root,
        &prepared.staging_root,
        remove_empty_data_root,
    )?;
    crate::AppPreferencesStore::new(prepared.data_root.join(super::PREFERENCES_PATH))
        .invalidate_observations();
    let mut transaction = BackupRestoreTransaction {
        data_root: prepared.data_root.clone(),
        rollback_root,
        journal,
        active: true,
        previous_runtime,
        lease,
    };
    for index in 0..LIVE_ITEMS.len() {
        if let Err(error) = transaction.journal.install(&prepared.data_root, index) {
            return Err(match transaction.rollback_in_place() {
                Ok(()) => {
                    BackupError::Transaction(format!("激活导入数据失败，原数据已恢复：{error}"))
                }
                Err(rollback) => BackupError::Transaction(format!(
                    "激活导入数据失败：{error}；恢复原数据也失败：{rollback}"
                )),
            });
        }
    }
    // Recovery owns staging cleanup from the first journal write onward.
    prepared.staging_root = PathBuf::new();
    Ok(transaction)
}

pub(super) fn rollback(transaction: &mut BackupRestoreTransaction) -> BackupResult<()> {
    crate::AppPreferencesStore::new(transaction.data_root.join(super::PREFERENCES_PATH))
        .invalidate_observations();
    transaction.journal.rollback(&transaction.data_root)
}

pub(super) fn create_unique_directory(parent: &Path, prefix: &str) -> BackupResult<PathBuf> {
    for _ in 0..128 {
        let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("{prefix}-{}-{sequence}", std::process::id()));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let mut builder = builder;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
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
