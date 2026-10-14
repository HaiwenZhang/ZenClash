//! Settings presentation adapter for independently owned backup commands.

use std::path::PathBuf;

use zenclash_core::BackupManager;

use super::super::super::{Page, load_page, settings::runtime_read::load_with_config};
use super::RestoreOutcome;
use crate::ApplicationCommands;

pub(super) enum RetryOutcome {
    Failed {
        runtime_version: u64,
        error: String,
    },
    Restored {
        runtime_version: u64,
        refresh: Result<Box<RetryRefresh>, String>,
    },
}

pub(super) struct RetryRefresh {
    pub(super) snapshot: zenclash_core::BackupDataSnapshot,
    pub(super) preferences_observation: zenclash_core::AppPreferencesObservation,
    pub(super) page_data: super::super::super::PageSnapshotData,
}

pub(super) async fn retry_runtime(
    application_commands: ApplicationCommands,
    data_root: Option<PathBuf>,
) -> RetryOutcome {
    let Some(data_root) = data_root else {
        return RetryOutcome::Failed {
            runtime_version: application_commands.session().generation(),
            error: zenclash_i18n::text("backup.errors.retry_state_unavailable"),
        };
    };
    let restoration = application_commands.backup_restoration(BackupManager::new(data_root));
    let receipt = match restoration.retry_runtime().await {
        Ok(receipt) => receipt,
        Err(error) => {
            return RetryOutcome::Failed {
                runtime_version: application_commands.session().generation(),
                error,
            };
        }
    };
    let runtime_version = receipt.runtime_generation();
    let refresh = async {
        let (snapshot, preferences_observation) = receipt.into_observed_snapshot()?.into_parts();
        let page_data = load_page(application_commands.client().clone(), Page::Settings).await?;
        Ok(Box::new(RetryRefresh {
            snapshot,
            preferences_observation,
            page_data,
        }))
    }
    .await;
    RetryOutcome::Restored {
        runtime_version,
        refresh,
    }
}

pub(super) async fn restore_backup(
    archive: PathBuf,
    application_commands: ApplicationCommands,
) -> Result<RestoreOutcome, String> {
    let manager = tokio::task::spawn_blocking(BackupManager::discover)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let restored = application_commands
        .backup_restoration(manager)
        .restore_archive(archive)
        .await?;
    // Presentation reads follow the committed business result and cannot roll it back.
    let page_data = load_with_config(restored.runtime_config()).await;
    Ok(RestoreOutcome {
        restored,
        page_data,
    })
}

pub(super) fn refresh_committed_state(outcome: RestoreOutcome) -> Result<RestoreOutcome, String> {
    Ok(RestoreOutcome {
        restored: outcome.restored.refresh_snapshot()?,
        page_data: outcome.page_data,
    })
}
