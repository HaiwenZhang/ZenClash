use std::path::PathBuf;

use zenclash_core::{BackupManager, ControlledConfigStore};

use super::super::ApplicationCommands;
use super::{BackupRecoveryReceipt, BackupRestoration};

impl BackupRestoration {
    /// Recovery and shared result acceptance belong to one independently owned completion.
    pub(crate) async fn retry_runtime(&self) -> Result<BackupRecoveryReceipt, String> {
        let restoration = self.clone();
        tokio::spawn(async move {
            let outcome = restoration
                .application_commands
                .session
                .retry_backup_restore()
                .await
                .map_err(|error| error.to_string())?;
            restoration
                .application_commands
                .record_accepted_runtime(&outcome);
            let manager: BackupManager = restoration.manager;
            let snapshot = tokio::task::spawn_blocking(move || manager.read_observed_snapshot())
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            Ok(BackupRecoveryReceipt::new(outcome.generation, snapshot))
        })
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.retry_task", &[("error", error.to_string())])
        })?
    }
}

pub(super) async fn rollback_restore<T>(
    transaction: zenclash_core::BackupRestoreTransaction,
    reason: String,
) -> Result<T, String> {
    let rollback = tokio::task::spawn_blocking(move || transaction.rollback())
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "backup.errors.rollback_task",
                &[("reason", reason.clone()), ("error", error.to_string())],
            )
        })?;
    match rollback {
        Ok(_) => Err(zenclash_i18n::text_with(
            "backup.errors.rolled_back",
            &[("reason", reason)],
        )),
        Err(error) => Err(zenclash_i18n::text_with(
            "backup.errors.rollback_failed",
            &[("reason", reason), ("error", error.to_string())],
        )),
    }
}

pub(super) async fn rollback_after_runtime_accept<T>(
    mut transaction: zenclash_core::BackupRestoreTransaction,
    commands: ApplicationCommands,
    data_root: PathBuf,
    reason: String,
    admission: &zenclash_core::CoreBackupAdmission,
) -> Result<T, String> {
    let (transaction, rollback) = tokio::task::spawn_blocking(move || {
        let rollback = transaction.rollback_in_place();
        (transaction, rollback)
    })
    .await
    .map_err(|error| {
        zenclash_i18n::text_with(
            "backup.errors.disk_rollback_task",
            &[("reason", reason.clone()), ("error", error.to_string())],
        )
    })?;
    if let Err(error) = rollback {
        return Err(zenclash_i18n::text_with(
            "backup.errors.disk_rollback_failed",
            &[("reason", reason), ("error", error.to_string())],
        ));
    }
    let Some(snapshot) = transaction.previous_runtime_snapshot() else {
        return Err(zenclash_i18n::text_with(
            "backup.errors.no_runtime_profile",
            &[("reason", reason)],
        ));
    };
    let (transaction, restored_state) = tokio::task::spawn_blocking(move || {
        let restored_state = (|| {
            let controlled = transaction
                .authorize_controlled_store(ControlledConfigStore::new(
                    data_root.join("controlled-config"),
                ))
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(controlled)
        })();
        (transaction, restored_state)
    })
    .await
    .map_err(|error| {
        zenclash_i18n::text_with(
            "backup.errors.override_read",
            &[("reason", reason.clone()), ("error", error.to_string())],
        )
    })?;
    let runtime_restore = match restored_state {
        Ok(controlled) => commands
            .session
            .restore_backup_snapshot(&controlled, &snapshot, admission)
            .await
            .inspect(|outcome| {
                commands.record_accepted_runtime(outcome);
            })
            .map_err(|error| error.to_string()),
        Err(error) => {
            return Err(zenclash_i18n::text_with(
                "backup.errors.override_read",
                &[("reason", reason), ("error", error)],
            ));
        }
    };
    drop(transaction);
    match runtime_restore {
        Ok(_) => Err(zenclash_i18n::text_with(
            "backup.errors.runtime_rolled_back",
            &[("reason", reason)],
        )),
        Err(error) => Err(zenclash_i18n::text_with(
            "backup.errors.runtime_rollback_failed",
            &[("reason", reason), ("error", error)],
        )),
    }
}
