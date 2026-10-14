use super::super::ApplicationCommands;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use zenclash_core::{
    AppPreferences, AppPreferencesStore, BackupManager, ControlledConfigStore, ProfileStore,
    YamlOverrideStore,
};
use zenclash_core::{AppearancePreference, CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

#[tokio::test]
async fn cancelled_backup_waiter_keeps_completion_owned_until_shutdown() {
    successful_backup(true, true).await;
}

#[tokio::test]
async fn committed_backup_returns_stores_with_live_ordinary_write_authority() {
    successful_backup(false, true).await;
}

#[tokio::test]
async fn failed_optional_controller_observation_keeps_the_committed_restore() {
    successful_backup(false, false).await;
}

async fn successful_backup(cancel_waiter: bool, readback_succeeds: bool) {
    let root = std::env::temp_dir().join(format!(
        "zenclash-backup-owned-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let target = root.join("target");
    for (data_root, port) in [(&source, 7890), (&target, 7891)] {
        AppPreferencesStore::new(data_root.join("preferences.json"))
            .save(&AppPreferences::default())
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
    let previous = ProfileStore::new(target.join("profiles"))
        .unwrap()
        .active_path()
        .unwrap()
        .unwrap();
    ControlledConfigStore::new(target.join("controlled-config"))
        .materialize(&previous)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut entered_tx = Some(entered_tx);
        let mut release_rx = Some(release_rx);
        for (index, response) in [
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (text, _body) = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_parts();
            if index == 0 {
                assert!(text.starts_with("PUT /configs"));
            } else {
                assert!(text.starts_with("GET /configs"));
                entered_tx.take().unwrap().send(()).unwrap();
                release_rx.take().unwrap().await.unwrap();
            }
            if index == 0 || readback_succeeds {
                stream.write_all(response).await.unwrap();
            }
            if index == 1 {
                break;
            }
        }
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(previous), Vec::new())
            .unwrap();
    let commands = ApplicationCommands::new(session.clone(), None);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let manager = BackupManager::new(&target);
    let restoration = commands.backup_restoration(manager);
    let waiter = tokio::spawn(async move { restoration.restore_archive(archive).await });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    if cancel_waiter {
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        let stopping = session.clone();
        let mut shutdown = tokio::spawn(async move { stopping.shutdown().await });
        let early = tokio::time::timeout(Duration::from_millis(100), &mut shutdown).await;
        let waited = early.is_err();
        release_tx.send(()).unwrap();
        server.await.unwrap();
        if let Ok(result) = early {
            result.unwrap().unwrap();
        } else {
            shutdown.await.unwrap().unwrap();
        }
        assert!(
            waited,
            "shutdown completed in the admitted backup readback/cleanup gap"
        );
        assert!(
            std::fs::read_to_string(
                ProfileStore::new(target.join("profiles"))
                    .unwrap()
                    .active_path()
                    .unwrap()
                    .unwrap()
            )
            .unwrap()
            .contains("7890")
        );
    } else {
        release_tx.send(()).unwrap();
        let outcome = waiter.await.unwrap().unwrap();
        assert_eq!(outcome.runtime_config().is_some(), readback_succeeds);
        assert!(
            outcome
                .profile_store()
                .active_path()
                .unwrap()
                .unwrap()
                .exists()
        );
        server.await.unwrap();
        let input = root.join("after.yaml");
        std::fs::write(&input, "mixed-port: 7892\nrules: [MATCH,DIRECT]\n").unwrap();
        tokio::task::spawn_blocking(move || {
            outcome.profile_store().import_local(&input).unwrap();
            outcome.controlled_store().materialize(&input).unwrap();
            outcome.override_store().import_paths(vec![input]).unwrap();
        })
        .await
        .unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_lost_reload_response_restores_the_exact_previous_runtime_before_releasing_data() {
    verify_ambiguous_restore(false).await;
}

async fn verify_ambiguous_restore(block_rollback: bool) {
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
    let (rollback_entered_tx, rollback_entered_rx) = tokio::sync::oneshot::channel();
    let (release_rollback_tx, release_rollback_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut rollback_entered_tx = Some(rollback_entered_tx);
        let mut release_rollback_rx = Some(release_rollback_rx);
        let mut payloads = Vec::new();
        for request_index in 0..2 {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let (headers, body) = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_parts();
            assert!(headers.starts_with("PUT /configs?force=true "));
            payloads.push(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["payload"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            if request_index == 1 {
                rollback_entered_tx.take().unwrap().send(()).unwrap();
                release_rollback_rx.take().unwrap().await.unwrap();
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
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(previous.clone()), Vec::new())
            .unwrap();
    std::fs::write(&previous, "mixed-port: 7899\nrules: [MATCH,REJECT]\n").unwrap();
    let commands = ApplicationCommands::new(session.clone(), None);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let manager = BackupManager::new(&target);
    let restoration = commands.backup_restoration(manager);
    let restoring = tokio::spawn(async move { restoration.restore_archive(archive).await });
    tokio::time::timeout(Duration::from_secs(5), rollback_entered_rx)
        .await
        .unwrap()
        .unwrap();
    let preferences = AppPreferencesStore::new(target.join("preferences.json"));
    assert_eq!(
        preferences.load().unwrap().appearance,
        AppearancePreference::Dark
    );
    let mut writer = block_rollback.then(|| {
        tokio::task::spawn_blocking(move || {
            preferences.update(|preferences| preferences.appearance = AppearancePreference::Light)
        })
    });
    let waited = if let Some(writer) = writer.as_mut() {
        tokio::time::timeout(Duration::from_millis(100), writer)
            .await
            .is_err()
    } else {
        false
    };
    release_rollback_tx.send(()).unwrap();
    assert!(restoring.await.unwrap().is_err());
    if let Some(writer) = writer {
        writer.await.unwrap().unwrap();
        assert!(waited, "writer committed between disk and runtime rollback");
    }
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
    assert!(!payloads[1].contains("7899"));
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        if block_rollback {
            AppearancePreference::Light
        } else {
            AppearancePreference::Dark
        }
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_writer_waits_for_the_http_runtime_rollback_after_disk_restoration() {
    verify_ambiguous_restore(true).await;
}
