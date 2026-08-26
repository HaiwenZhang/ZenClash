use std::path::PathBuf;

use zenclash_core::{BackupManager, WebDavBackup, WebDavService, WebDavSettings};

use super::super::super::profiles::workflow::CoreProfileRuntime;
use super::super::backup::{restore_prepared, RestoreOutcome};

pub(super) async fn restore_remote_backup(
    settings: WebDavSettings,
    filename: String,
    runtime: CoreProfileRuntime,
    previous_profile: Option<PathBuf>,
) -> Result<(RestoreOutcome, Vec<WebDavBackup>), String> {
    let service = WebDavService::new(settings).map_err(|error| error.to_string())?;
    let manager = tokio::task::spawn_blocking(BackupManager::discover)
        .await
        .map_err(|error| format!("本地备份目录任务异常结束：{error}"))?
        .map_err(|error| error.to_string())?;
    let prepared = service
        .prepare_restore(&manager, &filename)
        .await
        .map_err(|error| error.to_string())?;
    let backups = service
        .list_backups()
        .await
        .map_err(|error| error.to_string())?;
    // Finish all fallible remote reads before activating the local transaction.
    // Once restore_prepared succeeds, the in-memory UI state must be applied.
    let outcome = restore_prepared(manager, prepared, runtime, previous_profile).await?;
    Ok((outcome, backups))
}
