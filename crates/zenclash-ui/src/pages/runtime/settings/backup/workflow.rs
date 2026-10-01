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

    restore_prepared(manager, prepared, runtime).await
}

async fn restore_prepared(
    manager: BackupManager,
    prepared: PreparedBackupRestore,
    runtime: CoreProfileRuntime,
) -> Result<RestoreOutcome, String> {
    let file_count = prepared.file_count();
    let payload_bytes = prepared.payload_bytes();
    let session = runtime.session().clone();
    let transaction = tokio::task::spawn_blocking(move || {
        prepared
            .activate_for_session(&session)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| {
        zenclash_i18n::text_with(
            "backup.errors.activation_task",
            &[("error", error.to_string())],
        )
    })??;

    let root = manager.data_root().to_path_buf();
    let load_root = root.clone();
    let (transaction, loaded) = tokio::task::spawn_blocking(move || {
        let loaded = load_restored_state(&load_root, &transaction);
        (transaction, loaded)
    })
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
    let previous_runtime_version = runtime.session().generation();
    let runtime_version = match runtime
        .reload_with_overrides(controlled_store.clone(), &profile_path, overrides)
        .await
    {
        Ok(outcome) => outcome.generation,
        Err(error) => {
            let reason = zenclash_i18n::text_with(
                "backup.errors.core_rejected",
                &[
                    ("core", runtime.kind().display_name().to_owned()),
                    ("error", error),
                ],
            );
            if runtime.session().generation() != previous_runtime_version {
                return rollback_after_runtime_accept(transaction, runtime, root, reason).await;
            }
            return rollback_restore(transaction, reason).await;
        }
    };
    let page_data = match load_page(runtime.client().clone(), Page::Settings).await {
        Ok(data) => data,
        Err(error) => {
            let runtime_restore =
                zenclash_i18n::text_with("backup.errors.restored_settings", &[("error", error)]);
            return rollback_after_runtime_accept(transaction, runtime, root, runtime_restore)
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
        data_root: root,
        preferences,
        catalog,
        profile_store,
        controlled_store,
        controlled_config,
        override_store,
        override_catalog,
        runtime_version,
        page_data,
        file_count,
        payload_bytes,
        cleanup_warning,
    })
}

pub(super) fn refresh_committed_state(
    mut outcome: RestoreOutcome,
) -> Result<RestoreOutcome, String> {
    let snapshot = BackupManager::new(&outcome.data_root)
        .read_snapshot()
        .map_err(|error| error.to_string())?;
    outcome.preferences = snapshot.preferences;
    outcome.catalog = snapshot.profiles;
    outcome.controlled_config = snapshot.controlled_config;
    outcome.override_catalog = snapshot.overrides;
    Ok(outcome)
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

fn load_restored_state(
    root: &Path,
    transaction: &zenclash_core::BackupRestoreTransaction,
) -> Result<RestoredState, String> {
    let preferences = AppPreferencesStore::new(root.join("preferences.json"))
        .load()
        .map_err(|error| error.to_string())?;
    let profile_store = transaction
        .profile_store()
        .map_err(|error| error.to_string())?;
    let catalog = profile_store.load().map_err(|error| error.to_string())?;
    let profile_path = profile_store
        .active_path()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| zenclash_i18n::text("backup.errors.missing_profile"))?;
    let controlled_store = transaction
        .authorize_controlled_store(ControlledConfigStore::new(root.join("controlled-config")))
        .map_err(|error| error.to_string())?;
    let controlled_config = controlled_store
        .load_json()
        .map_err(|error| error.to_string())?;
    let override_store = transaction
        .yaml_override_store()
        .map_err(|error| error.to_string())?;
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

async fn rollback_after_runtime_accept<T>(
    mut transaction: zenclash_core::BackupRestoreTransaction,
    runtime: CoreProfileRuntime,
    data_root: PathBuf,
    reason: String,
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
        Ok(controlled) => runtime.restore_snapshot(&controlled, &snapshot).await,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use zenclash_core::{
        AppearancePreference, CoreKind, CoreSession, MihomoClient, MihomoEndpoint,
    };

    #[tokio::test]
    async fn a_lost_reload_response_restores_the_exact_previous_runtime_before_releasing_data() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-backup-ambiguous-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        for (data_root, port, appearance) in [
            (&source, 7890, AppearancePreference::Light),
            (&target, 7891, AppearancePreference::Dark),
        ] {
            AppPreferencesStore::new(data_root.join("preferences.json"))
                .save(&AppPreferences {
                    appearance,
                    ..AppPreferences::default()
                })
                .unwrap();
            let input = data_root.join("input.yaml");
            std::fs::write(
                &input,
                format!("mixed-port: {port}\nrules: [MATCH,DIRECT]\n"),
            )
            .unwrap();
            let profiles = ProfileStore::new(data_root.join("profiles")).unwrap();
            let profile = profiles.import_local(input).unwrap();
            profiles.activate(&profile.id).unwrap();
            YamlOverrideStore::new(data_root.join("yaml-overrides")).unwrap();
        }
        let controlled = ControlledConfigStore::new(target.join("controlled-config"));
        let previous = ProfileStore::new(target.join("profiles"))
            .unwrap()
            .active_path()
            .unwrap()
            .unwrap();
        controlled.materialize(&previous).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut payloads = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                loop {
                    request.push(stream.read_u8().await.unwrap());
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let headers = String::from_utf8(request).unwrap();
                assert!(headers.starts_with("PUT /configs?force=true "));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                payloads.push(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["payload"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                if request_index == 1 {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
                // The first configuration has been accepted by the controller;
                // losing its HTTP response leaves the caller's result uncertain.
            }
            payloads
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            None,
            Some(previous.clone()),
            Vec::new(),
        );
        let runtime = CoreProfileRuntime::new(session.clone(), None);
        let archive = root.join("backup.zip");
        BackupManager::new(&source).export_to(&archive).unwrap();
        let manager = BackupManager::new(&target);
        let prepared = manager.prepare_restore(&archive).unwrap();
        assert!(restore_prepared(manager, prepared, runtime).await.is_err());
        assert_eq!(
            session.generation(),
            2,
            "uncertain imported runtime was not restored"
        );
        assert_eq!(
            session.committed_profile_snapshot().profile_path,
            Some(previous)
        );
        let payloads = server.await.unwrap();
        assert!(payloads[0].contains("7890"));
        assert!(payloads[1].contains("7891"));
        assert_eq!(
            AppPreferencesStore::new(target.join("preferences.json"))
                .load()
                .unwrap()
                .appearance,
            AppearancePreference::Dark
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_writer_waits_for_the_http_runtime_rollback_after_disk_restoration() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-backup-runtime-rollback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let target = root.join("target");
        AppPreferencesStore::new(source.join("preferences.json"))
            .save(&AppPreferences::default())
            .unwrap();
        let preferences = AppPreferencesStore::new(target.join("preferences.json"));
        preferences
            .save(&AppPreferences {
                appearance: AppearancePreference::Dark,
                ..AppPreferences::default()
            })
            .unwrap();
        let profile = root.join("previous.yaml");
        std::fs::write(&profile, "mixed-port: 7891\nrules: [MATCH,DIRECT]\n").unwrap();
        ControlledConfigStore::new(target.join("controlled-config"))
            .materialize(&profile)
            .unwrap();
        // Imported sources can change after acceptance. Rollback must replay
        // the committed cache instead of regenerating from these current bytes.
        std::fs::write(&profile, "mixed-port: 7899\nrules: [MATCH,REJECT]\n").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (respond_tx, respond_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count]).starts_with("PUT /configs?force=true ")
            );
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("7891"));
            assert!(!request.contains("7899"));
            accepted_tx.send(()).unwrap();
            respond_rx.await.unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            None,
            Some(profile.clone()),
            Vec::new(),
        );
        let runtime = CoreProfileRuntime::new(session.clone(), None);
        let archive = root.join("backup.zip");
        BackupManager::new(&source).export_to(&archive).unwrap();
        let prepared = BackupManager::new(&target)
            .prepare_restore(&archive)
            .unwrap();
        let activation_session = session.clone();
        let transaction =
            tokio::task::spawn_blocking(move || prepared.activate_for_session(&activation_session))
                .await
                .unwrap()
                .unwrap();
        let rollback = tokio::spawn(rollback_after_runtime_accept::<()>(
            transaction,
            runtime,
            target.clone(),
            "settings refresh failed".into(),
        ));
        tokio::time::timeout(Duration::from_secs(5), accepted_rx)
            .await
            .unwrap()
            .unwrap();
        // The disk is already restored and the controller is processing the
        // previous payload. A sibling store must remain excluded until its reply.
        assert_eq!(
            preferences.load().unwrap().appearance,
            AppearancePreference::Dark
        );
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut writer = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            preferences.update(|preferences| preferences.appearance = AppearancePreference::Light)
        });
        started_rx.await.unwrap();
        let early = tokio::time::timeout(Duration::from_millis(100), &mut writer).await;
        let waited = early.is_err();
        respond_tx.send(()).unwrap();
        server.await.unwrap();
        assert!(rollback.await.unwrap().is_err());
        if let Ok(result) = early {
            result.unwrap().unwrap();
        } else {
            writer.await.unwrap().unwrap();
        }
        assert!(
            waited,
            "ordinary writer committed between disk and runtime rollback"
        );
        assert_eq!(session.generation(), 1);
        assert_eq!(
            AppPreferencesStore::new(target.join("preferences.json"))
                .load()
                .unwrap()
                .appearance,
            AppearancePreference::Light
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
