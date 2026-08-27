use std::path::{Path, PathBuf};

use zenclash_core::{
    AppPreferences, AppPreferencesStore, BackupManager, ControlledConfigStore,
    PreparedBackupRestore, ProfileCatalog, ProfileStore, YamlOverrideCatalog, YamlOverrideStore,
};

use super::super::super::profiles::workflow::CoreProfileRuntime;
use super::super::super::{Page, load_page};
use super::RestoreOutcome;

pub(super) async fn restore_backup(
    archive: PathBuf,
    runtime: CoreProfileRuntime,
    previous_profile: Option<PathBuf>,
) -> Result<RestoreOutcome, String> {
    let (manager, prepared) = tokio::task::spawn_blocking(move || {
        let manager = BackupManager::discover().map_err(|error| error.to_string())?;
        let prepared = manager
            .prepare_restore(archive)
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((manager, prepared))
    })
    .await
    .map_err(|error| {
        zenclash_i18n::text_with(
            "backup.errors.validation_task",
            &[("error", error.to_string())],
        )
    })??;

    restore_prepared(manager, prepared, runtime, previous_profile).await
}

async fn restore_prepared(
    manager: BackupManager,
    prepared: PreparedBackupRestore,
    runtime: CoreProfileRuntime,
    previous_profile: Option<PathBuf>,
) -> Result<RestoreOutcome, String> {
    let file_count = prepared.file_count();
    let payload_bytes = prepared.payload_bytes();
    let transaction =
        tokio::task::spawn_blocking(move || prepared.activate().map_err(|error| error.to_string()))
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "backup.errors.activation_task",
                    &[("error", error.to_string())],
                )
            })??;

    let root = manager.data_root().to_path_buf();
    let load_root = root.clone();
    let loaded = tokio::task::spawn_blocking(move || load_restored_state(&load_root))
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.state_task", &[("error", error.to_string())])
        })?;
    let (
        preferences,
        catalog,
        profile_store,
        controlled_store,
        controlled_config,
        override_store,
        override_catalog,
        profile_path,
    ) = match loaded {
        Ok(state) => state,
        Err(error) => return rollback_restore(transaction, error).await,
    };

    let overrides = override_store.enabled_paths(&override_catalog);
    if let Err(error) = runtime
        .reload_with_overrides(controlled_store.clone(), &profile_path, overrides)
        .await
    {
        return rollback_restore(
            transaction,
            zenclash_i18n::text_with(
                "backup.errors.core_rejected",
                &[
                    ("core", runtime.kind().display_name().to_owned()),
                    ("error", error),
                ],
            ),
        )
        .await;
    }
    let page_data = match load_page(runtime.client().clone(), Page::Settings).await {
        Ok(data) => data,
        Err(error) => {
            let runtime_restore =
                zenclash_i18n::text_with("backup.errors.restored_settings", &[("error", error)]);
            return rollback_after_runtime_accept(
                transaction,
                runtime,
                previous_profile,
                root,
                runtime_restore,
            )
            .await;
        }
    };
    let cleanup_warning = tokio::task::spawn_blocking(move || transaction.commit())
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("backup.errors.commit_task", &[("error", error.to_string())])
        })?
        .err()
        .map(|error| error.to_string());
    Ok(RestoreOutcome {
        preferences,
        catalog,
        profile_store,
        controlled_store,
        controlled_config,
        override_store,
        override_catalog,
        profile_path,
        page_data,
        file_count,
        payload_bytes,
        cleanup_warning,
    })
}

type RestoredState = (
    AppPreferences,
    ProfileCatalog,
    ProfileStore,
    ControlledConfigStore,
    serde_json::Value,
    YamlOverrideStore,
    YamlOverrideCatalog,
    PathBuf,
);

fn load_restored_state(root: &Path) -> Result<RestoredState, String> {
    let preferences = AppPreferencesStore::new(root.join("preferences.json"))
        .load()
        .map_err(|error| error.to_string())?;
    let profile_store =
        ProfileStore::new(root.join("profiles")).map_err(|error| error.to_string())?;
    let catalog = profile_store.load().map_err(|error| error.to_string())?;
    let profile_path = profile_store
        .active_path()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| zenclash_i18n::text("backup.errors.missing_profile"))?;
    let controlled_store = ControlledConfigStore::new(root.join("controlled-config"));
    let controlled_config = controlled_store
        .load_json()
        .map_err(|error| error.to_string())?;
    let override_store =
        YamlOverrideStore::new(root.join("yaml-overrides")).map_err(|error| error.to_string())?;
    let override_catalog = override_store.load().map_err(|error| error.to_string())?;
    Ok((
        preferences,
        catalog,
        profile_store,
        controlled_store,
        controlled_config,
        override_store,
        override_catalog,
        profile_path,
    ))
}

async fn rollback_restore<T>(
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
        Ok(()) => Err(zenclash_i18n::text_with(
            "backup.errors.rolled_back",
            &[("reason", reason)],
        )),
        Err(error) => Err(zenclash_i18n::text_with(
            "backup.errors.rollback_failed",
            &[("reason", reason), ("error", error.to_string())],
        )),
    }
}

async fn rollback_after_runtime_accept<T>(
    transaction: zenclash_core::BackupRestoreTransaction,
    runtime: CoreProfileRuntime,
    previous_profile: Option<PathBuf>,
    data_root: PathBuf,
    reason: String,
) -> Result<T, String> {
    let rollback = tokio::task::spawn_blocking(move || transaction.rollback())
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
    let Some(previous_profile) = previous_profile else {
        return Err(zenclash_i18n::text_with(
            "backup.errors.no_runtime_profile",
            &[("reason", reason)],
        ));
    };
    let controlled = ControlledConfigStore::new(data_root.join("controlled-config"));
    let overrides = YamlOverrideStore::new(data_root.join("yaml-overrides"))
        .and_then(|store| store.load_enabled_paths())
        .map_err(|error| error.to_string());
    let runtime_restore = match overrides {
        Ok(overrides) => {
            runtime
                .reload_with_overrides(controlled, &previous_profile, overrides)
                .await
        }
        Err(error) => {
            return Err(zenclash_i18n::text_with(
                "backup.errors.override_read",
                &[("reason", reason), ("error", error)],
            ));
        }
    };
    match runtime_restore {
        Ok(()) => Err(zenclash_i18n::text_with(
            "backup.errors.runtime_rolled_back",
            &[("reason", reason)],
        )),
        Err(error) => Err(zenclash_i18n::text_with(
            "backup.errors.runtime_rollback_failed",
            &[("reason", reason), ("error", error)],
        )),
    }
}
