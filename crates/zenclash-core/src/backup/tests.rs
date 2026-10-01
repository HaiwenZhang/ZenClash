use std::{
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::{
    AppPreferences, AppearancePreference, ControlledConfigStore, ProfileStore, YamlOverrideStore,
    profiles::atomic_write,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PROFILE: &str = "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n";

#[test]
fn preference_writer_waits_until_restore_commits() {
    verify_writer_waits_for_restore(false);
}

#[test]
fn committed_snapshot_read_waits_for_restore_commit_or_rollback() {
    use std::{sync::mpsc, thread, time::Duration};

    for rollback in [false, true] {
        let root = test_root("committed-read");
        let source = root.join("source");
        let target = root.join("target");
        create_snapshot(&source, AppearancePreference::Light, false, 7890);
        create_snapshot(&target, AppearancePreference::Dark, true, 7891);
        let archive = root.join("backup.zip");
        BackupManager::new(&source).export_to(&archive).unwrap();
        let transaction = BackupManager::new(&target)
            .prepare_restore(&archive)
            .unwrap()
            .activate()
            .unwrap();
        let manager = BackupManager::new(&target);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            started_tx.send(()).unwrap();
            finished_tx.send(manager.read_snapshot()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let early = finished_rx.recv_timeout(Duration::from_millis(100));
        let waited = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
        if rollback {
            transaction.rollback().unwrap();
        } else {
            transaction.commit().unwrap();
        }
        let snapshot = early
            .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .unwrap();
        reader.join().unwrap();
        assert!(
            waited,
            "business synchronization observed an uncommitted restore"
        );
        assert_eq!(
            snapshot.preferences.appearance,
            if rollback {
                AppearancePreference::Dark
            } else {
                AppearancePreference::Light
            }
        );
        assert_eq!(
            snapshot.controlled_config["mixed-port"],
            if rollback { 7891 } else { 7890 }
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn dropping_restore_releases_waiting_writer_after_restoring_previous_data() {
    verify_writer_waits_for_restore(true);
}

#[test]
fn disk_rollback_retains_exclusive_authority_until_runtime_cache_reconciliation_ends() {
    use std::{sync::mpsc, thread, time::Duration};

    let root = test_root("in-place-rollback-runtime");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let original = read_authoritative_snapshot(&target);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let mut transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    transaction.rollback_in_place().unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original);

    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let writer_root = target.clone();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        finished_tx
            .send(
                AppPreferencesStore::new(writer_root.join("preferences.json"))
                    .update(|preferences| preferences.appearance = AppearancePreference::Light),
            )
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = finished_rx.recv_timeout(Duration::from_millis(100));
    let waited = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
    let profile = transaction
        .profile_store()
        .unwrap()
        .active_path()
        .unwrap()
        .unwrap();
    let controlled = transaction
        .authorize_controlled_store(ControlledConfigStore::new(target.join("controlled-config")))
        .unwrap();
    controlled.materialize(profile).unwrap();
    let cache = fs::read(controlled.runtime_path()).unwrap();
    transaction.rollback_in_place().unwrap();
    assert_eq!(fs::read(controlled.runtime_path()).unwrap(), cache);
    drop(transaction);
    early
        .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    writer.join().unwrap();
    assert!(
        waited,
        "writer committed before runtime reconciliation ended"
    );
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        AppearancePreference::Light
    );
    assert_eq!(fs::read(controlled.runtime_path()).unwrap(), cache);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_preexisting_preferences_symlink_cannot_bypass_the_root_restore_lease() {
    use std::{os::unix::fs::symlink, sync::mpsc, thread, time::Duration};

    let root = test_root("symlink-preferences-writer");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let external = root.join("external-preferences.json");
    let original = fs::read(target.join("preferences.json")).unwrap();
    fs::write(&external, &original).unwrap();
    fs::remove_file(target.join("preferences.json")).unwrap();
    symlink(&external, target.join("preferences.json")).unwrap();
    let preferences = AppPreferencesStore::new(target.join("preferences.json"));
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        finished_tx
            .send(preferences.update(|preferences| preferences.traffic_tray_visible = true))
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = finished_rx.recv_timeout(Duration::from_millis(100));
    let waited = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
    transaction.commit().unwrap();
    early
        .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    writer.join().unwrap();
    assert!(
        waited,
        "atomic destination escaped the restore lease through a symlink"
    );
    assert_eq!(fs::read(&external).unwrap(), original);
    assert!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .traffic_tray_visible
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn rolling_back_a_restore_preserves_a_preexisting_dangling_preferences_symlink() {
    use std::os::unix::fs::symlink;

    let root = test_root("dangling-preferences-rollback");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let preferences = target.join("preferences.json");
    let missing = root.join("missing-preferences.json");
    fs::remove_file(&preferences).unwrap();
    symlink(&missing, &preferences).unwrap();
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap()
        .rollback()
        .unwrap();
    let restored = fs::read_link(&preferences);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(restored.unwrap(), missing);
}

#[cfg(unix)]
#[test]
fn export_follows_managed_directory_symlinks_and_restores_an_independent_snapshot() {
    use std::os::unix::fs::symlink;

    let root = test_root("symlink-export");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    for item in [
        "profiles/files",
        "yaml-overrides/files",
        "profiles",
        "yaml-overrides",
    ] {
        let physical = root.join(format!("external-{}", item.replace('/', "-")));
        fs::rename(source.join(item), &physical).unwrap();
        symlink(&physical, source.join(item)).unwrap();
    }
    let original = read_authoritative_snapshot(&source);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original);
    assert!(
        !fs::symlink_metadata(target.join("profiles"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !fs::symlink_metadata(target.join("yaml-overrides"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn managed_file_directory_aliases_obey_the_other_roots_restore_lease() {
    use std::{os::unix::fs::symlink, sync::mpsc, thread, time::Duration};

    for overrides in [false, true] {
        for restore_owns_alias in [false, true] {
            let root = test_root("subtree-restore-writer");
            let source = root.join("source");
            let target = root.join("target");
            let other = root.join("other");
            create_snapshot(&source, AppearancePreference::Light, false, 7890);
            create_snapshot(&target, AppearancePreference::Dark, true, 7891);
            create_snapshot(&other, AppearancePreference::Dark, false, 7892);
            let store_name = if overrides {
                "yaml-overrides"
            } else {
                "profiles"
            };
            let target_files = target.join(store_name).join("files");
            let other_files = other.join(store_name).join("files");
            let (alias, destination) = if restore_owns_alias {
                (&target_files, &other_files)
            } else {
                (&other_files, &target_files)
            };
            for entry in fs::read_dir(alias).unwrap() {
                let entry = entry.unwrap();
                fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
            }
            fs::remove_dir_all(alias).unwrap();
            symlink(destination, alias).unwrap();
            let profiles = ProfileStore::new(other.join("profiles")).unwrap();
            let yaml = YamlOverrideStore::new(other.join("yaml-overrides")).unwrap();
            let archive = root.join("backup.zip");
            BackupManager::new(&source).export_to(&archive).unwrap();
            let transaction = BackupManager::new(&target)
                .prepare_restore(&archive)
                .unwrap()
                .activate()
                .unwrap();
            let input = root.join("writer.yaml");
            fs::write(&input, PROFILE).unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let (finished_tx, finished_rx) = mpsc::channel();
            let writer = thread::spawn(move || {
                started_tx.send(()).unwrap();
                let result = if overrides {
                    yaml.import_paths([input])
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                } else {
                    profiles
                        .import_local(input)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                };
                finished_tx.send(result).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let early = finished_rx.recv_timeout(Duration::from_millis(100));
            let waited = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
            drop(transaction);
            let result =
                early.unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap());
            writer.join().unwrap();
            assert!(
                waited,
                "managed files crossed the other root's active restore"
            );
            result.unwrap_or_else(|error| {
                panic!("family={store_name}, restore_owns_alias={restore_owns_alias}: {error}")
            });
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[tokio::test]
async fn runtime_restore_snapshot_is_captured_after_admission_and_replays_exact_bytes() {
    use crate::{CoreKind, CoreSession, EffectiveConfigIntent, MihomoClient, MihomoEndpoint};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = test_root("exact-runtime-snapshot");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let controlled = ControlledConfigStore::new(target.join("controlled-config"));
    let previous = ProfileStore::new(target.join("profiles"))
        .unwrap()
        .active_path()
        .unwrap()
        .unwrap();
    controlled.materialize(&previous).unwrap();
    let candidate = root.join("candidate.yaml");
    let overlay = root.join("overlay.yaml");
    fs::write(&candidate, "mixed-port: 7991\nrules: ['MATCH,DIRECT']\n").unwrap();
    fs::write(&overlay, "mode: global\n").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut entered = Some(entered_tx);
        let mut release = Some(release_rx);
        let mut payloads = Vec::new();
        for request_index in 0..2 {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
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
            if request_index == 0 {
                entered.take().unwrap().send(()).unwrap();
                release.take().unwrap().await.unwrap();
            }
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
        payloads
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, None, Some(previous), Vec::new());
    let apply_session = session.clone();
    let apply_controlled = controlled.clone();
    let apply_profile = candidate.clone();
    let apply_overlay = overlay.clone();
    let apply = tokio::spawn(async move {
        apply_session
            .apply(
                &apply_controlled,
                EffectiveConfigIntent::ActivateProfile {
                    profile: apply_profile,
                    overrides: vec![apply_overlay],
                },
            )
            .await
    });
    entered_rx.await.unwrap();
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let prepared = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap();
    let activation_session = session.clone();
    let mut activation =
        tokio::task::spawn_blocking(move || prepared.activate_for_session(&activation_session));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut activation)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    assert_eq!(apply.await.unwrap().unwrap().generation, 1);
    let mut transaction = activation.await.unwrap().unwrap();
    let snapshot = transaction.previous_runtime_snapshot().unwrap();
    fs::write(&candidate, "mixed-port: 7999\nrules: ['MATCH,REJECT']\n").unwrap();
    fs::write(&overlay, "mode: direct\n").unwrap();
    transaction.rollback_in_place().unwrap();
    let authorized = transaction.authorize_controlled_store(controlled).unwrap();
    assert_eq!(
        session
            .restore_snapshot(&authorized, &snapshot)
            .await
            .unwrap()
            .generation,
        2
    );
    drop(transaction);
    let payloads = server.await.unwrap();
    assert_eq!(
        payloads[0], payloads[1],
        "runtime restore regenerated changed source or overrides"
    );
    let restored: serde_yaml::Value = serde_yaml::from_str(&payloads[1]).unwrap();
    assert_eq!(restored["mixed-port"], 7891);
    assert_eq!(restored["mode"], "global");
    assert_eq!(restored["rules"][0], "MATCH,DIRECT");
    assert_eq!(
        session.committed_profile_snapshot().profile_path,
        Some(candidate)
    );
    fs::remove_dir_all(root).unwrap();
}

fn verify_writer_waits_for_restore(rollback: bool) {
    use std::{sync::mpsc, thread, time::Duration};

    let root = test_root(if rollback {
        "writer-rollback"
    } else {
        "writer-commit"
    });
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let preferences = AppPreferencesStore::new(target.join("preferences.json"));
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let expected = if rollback {
        AppearancePreference::Light
    } else {
        AppearancePreference::Dark
    };
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        finished_tx
            .send(preferences.update(|preferences| preferences.appearance = expected))
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = finished_rx.recv_timeout(Duration::from_millis(100));
    let waited = matches!(&early, Err(mpsc::RecvTimeoutError::Timeout));
    if rollback {
        drop(transaction);
    } else {
        transaction.commit().unwrap();
    }
    early
        .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    writer.join().unwrap();
    let actual = AppPreferencesStore::new(target.join("preferences.json"))
        .load()
        .unwrap()
        .appearance;
    fs::remove_dir_all(root).unwrap();
    assert!(
        waited,
        "writer committed inside the exclusive restore transaction"
    );
    assert_eq!(
        actual, expected,
        "restore discarded a successful writer update"
    );
}

#[test]
fn restore_authority_allows_cache_writes_and_expires_after_commit() {
    use std::{sync::mpsc, thread, time::Duration};

    let root = test_root("restore-authority-expiry");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let profile = root.join("candidate.yaml");
    fs::write(&profile, PROFILE).unwrap();
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let manager = BackupManager::new(&target);
    let transaction = manager
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let controlled = transaction
        .authorize_controlled_store(ControlledConfigStore::new(target.join("controlled-config")))
        .unwrap();
    // A runtime cache is deliberately rebuilt before restore commits.
    controlled.materialize(&profile).unwrap();
    assert!(controlled.runtime_path().is_file());
    transaction.commit().unwrap();

    let transaction = manager
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let runtime_path = controlled.runtime_path();
    let writer = thread::spawn(move || {
        started_tx.send(()).unwrap();
        finished_tx.send(controlled.materialize(profile)).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = finished_rx.recv_timeout(Duration::from_millis(100));
    let waited = matches!(&early, Err(mpsc::RecvTimeoutError::Timeout));
    drop(transaction);
    early
        .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    writer.join().unwrap();
    assert!(waited, "escaped restore authority bypassed a later restore");
    assert!(runtime_path.is_file());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn session_restore_authorizes_profile_application_in_an_external_runtime_home() {
    use crate::{
        CoreConfigValidator, CoreKind, CoreSession, MihomoClient, MihomoEndpoint,
        ProfileApplication, ProfileApplyOutcome, ProfileChange,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        thread,
        time::Duration,
    };

    let root = test_root("external-runtime-authority");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let profile = root.join("candidate.yaml");
    fs::write(&profile, PROFILE).unwrap();
    let binary = root.join("validator");
    fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let home = root.join("external-runtime-home");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("controller accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0_u8; 8192];
        let count = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..count]).starts_with("PUT /configs?force=true "));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let client = MihomoClient::new(endpoint)
        .unwrap()
        .with_config_validator(CoreConfigValidator::new(CoreKind::Mihomo, binary, &home));
    let session = CoreSession::open(CoreKind::Mihomo, client.clone(), None);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate_for_session(&session)
        .unwrap();
    let controlled = transaction
        .authorize_controlled_store(ControlledConfigStore::new(target.join("controlled-config")))
        .unwrap();
    let profile_store = transaction.profile_store().unwrap();
    let id = profile_store.load().unwrap().active.unwrap();
    let application = ProfileApplication::new(profile_store, controlled, session);
    let applied = tokio::time::timeout(
        Duration::from_secs(5),
        application.apply(ProfileChange::ActivateExisting {
            id,
            overrides: Vec::new(),
        }),
    )
    .await;
    transaction.commit().unwrap();
    server.join().unwrap();
    assert!(matches!(
        applied.unwrap(),
        ProfileApplyOutcome::Applied { .. }
    ));
    assert!(home.is_dir());
    assert!(
        fs::read_dir(&home).unwrap().next().is_none(),
        "temporary validation config was retained"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restoring_one_root_does_not_block_writes_to_an_independent_root() {
    use std::{sync::mpsc, thread, time::Duration};

    let root = test_root("independent-root-writer");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let other = root.join("independent/preferences.json");
    let (finished_tx, finished_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        finished_tx
            .send(AppPreferencesStore::new(other).update(|preferences| {
                preferences.appearance = AppearancePreference::Light;
            }))
            .unwrap();
    });
    let result = finished_rx.recv_timeout(Duration::from_secs(5));
    drop(transaction);
    writer.join().unwrap();
    result.unwrap().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_activation_releases_the_writer_lease_after_restoring_previous_data() {
    use std::{sync::mpsc, thread, time::Duration};

    let root = test_root("activation-failure-writer");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let prepared = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap();
    fs::remove_dir_all(prepared.staging_root.join("profiles")).unwrap();
    assert!(prepared.activate().is_err());
    let (finished_tx, finished_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        finished_tx
            .send(
                AppPreferencesStore::new(target.join("preferences.json"))
                    .update(|preferences| preferences.appearance = AppearancePreference::Light),
            )
            .unwrap();
    });
    finished_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_rejects_an_index_whose_pretty_encoding_exceeds_the_store_capacity() {
    let root = test_root("catalog-export-capacity");
    let source = root.join("source");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    let store = ProfileStore::new(source.join("profiles")).unwrap();
    let mut catalog = store.load().unwrap();
    let limit = 4 * 1024 * 1024;
    catalog.profiles[0].name = "x".repeat(limit);
    let overhead = serde_json::to_vec(&catalog).unwrap().len() - limit;
    catalog.profiles[0].name.truncate(limit - overhead - 1);
    fs::write(
        source.join(PROFILE_INDEX_PATH),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    assert!(store.load().is_ok());
    let destination = root.join("backup.zip");
    assert!(matches!(
        BackupManager::new(&source).export_to(&destination),
        Err(BackupError::Profiles(
            ProfileStoreError::IndexTooLarge { .. }
        ))
    ));
    assert!(!destination.exists());
    assert!(store.load().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_an_oversized_valid_index_without_changing_live_data() {
    use sha2::{Digest, Sha256};

    let root = test_root("catalog-restore-capacity");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let before = read_authoritative_snapshot(&target);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let mut catalog = ProfileStore::new(source.join("profiles"))
        .unwrap()
        .load()
        .unwrap();
    catalog.profiles[0].name = "x".repeat(4 * 1024 * 1024);
    let index = serde_json::to_vec(&catalog).unwrap();
    let oversized = root.join("oversized.zip");
    rewrite_archive(&archive, &oversized, |name, bytes| {
        if name == PROFILE_INDEX_PATH {
            *bytes = index.clone();
        }
        if name == MANIFEST_PATH {
            let mut manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            let entry = manifest["files"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["path"] == PROFILE_INDEX_PATH)
                .unwrap();
            entry["size"] = serde_json::json!(index.len());
            entry["sha256"] = serde_json::json!(format!("{:x}", Sha256::digest(&index)));
            *bytes = serde_json::to_vec(&manifest).unwrap();
        }
    });
    assert!(matches!(
        BackupManager::new(&target).prepare_restore(&oversized),
        Err(BackupError::Profiles(
            ProfileStoreError::IndexTooLarge { .. }
        ))
    ));
    assert_eq!(read_authoritative_snapshot(&target), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_restore_is_complete_reversible_and_excludes_generated_cache() {
    let root = test_root("roundtrip");
    let source = root.join("source");
    let target = root.join("target");
    let archive = root.join("backup.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    fs::write(
        source.join("controlled-config/effective.yaml"),
        "mixed-port: 6553\n",
    )
    .unwrap();

    let summary = BackupManager::new(&source).export_to(&archive).unwrap();

    assert_eq!(summary.file_count, 6);
    assert!(summary.payload_bytes > 0);
    assert!(
        !archive_names(&archive)
            .iter()
            .any(|name| name.ends_with("effective.yaml"))
    );
    let original_target = read_authoritative_snapshot(&target);
    let prepared = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original_target);

    let transaction = prepared.activate().unwrap();
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        AppearancePreference::Light
    );
    assert!(
        ControlledConfigStore::new(target.join("controlled-config"))
            .load_json()
            .unwrap()["mixed-port"]
            .as_u64()
            .is_some_and(|port| port == 17_890)
    );
    let restored_overrides = transaction.yaml_override_store().unwrap().load().unwrap();
    assert_eq!(restored_overrides.items.len(), 1);
    transaction.rollback().unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original_target);

    BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        AppearancePreference::Light
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn rollback_cleanup_failure_keeps_restored_originals_when_the_transaction_drops() {
    use std::os::unix::fs::PermissionsExt;

    let root = test_root("rollback-cleanup-failure");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    let original = read_authoritative_snapshot(&target);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();

    // Item renames remain possible inside the writable child directories, while
    // removing the now-empty rollback directory requires a writable parent.
    let permissions = fs::metadata(&root).unwrap().permissions();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let result = transaction.rollback();
    fs::set_permissions(&root, permissions).unwrap();

    assert!(result.is_err(), "the cleanup failure was not exercised");
    assert_eq!(read_authoritative_snapshot(&target), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_retries_only_remaining_items_after_a_partial_restore() {
    let root = test_root("rollback-partial-restore");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    let original = read_authoritative_snapshot(&target);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let mut transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    let unavailable = root.join("temporarily-unavailable-profiles");
    fs::rename(transaction.rollback_root.join("profiles"), &unavailable).unwrap();

    assert!(transaction::rollback(&mut transaction).is_err());
    // YAML overrides precede profiles in reverse restoration order. A retry
    // must leave that restored tree in place when the missing item reappears.
    let restored_override = fs::read(target.join("yaml-overrides/overrides.json")).unwrap();
    fs::rename(&unavailable, transaction.rollback_root.join("profiles")).unwrap();
    transaction.rollback().unwrap();

    assert_eq!(
        fs::read(target.join("yaml-overrides/overrides.json")).unwrap(),
        restored_override
    );
    assert_eq!(read_authoritative_snapshot(&target), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_removes_a_data_directory_that_did_not_exist_before_restore() {
    let root = test_root("rollback-new-data-directory");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let transaction = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap();
    assert!(target.is_dir());

    transaction.rollback().unwrap();

    assert!(!target.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_checksum_mismatch_before_touching_live_data() {
    let root = test_root("checksum");
    let source = root.join("source");
    let target = root.join("target");
    let archive = root.join("backup.zip");
    let tampered = root.join("tampered.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    BackupManager::new(&source).export_to(&archive).unwrap();
    rewrite_archive(&archive, &tampered, |name, bytes| {
        if name == PREFERENCES_PATH {
            bytes.push(b' ');
        }
    });
    let previous = read_authoritative_snapshot(&target);

    let error = BackupManager::new(&target)
        .prepare_restore(&tampered)
        .unwrap_err();

    assert!(error.to_string().contains("SHA-256"));
    assert_eq!(read_authoritative_snapshot(&target), previous);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_zip_slip_paths() {
    let root = test_root("zip-slip");
    fs::create_dir_all(&root).unwrap();
    let archive = root.join("unsafe.zip");
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    writer
        .start_file("../preferences.json", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"{}").unwrap();
    fs::write(&archive, writer.finish().unwrap().into_inner()).unwrap();

    let error = BackupManager::new(root.join("live"))
        .prepare_restore(&archive)
        .unwrap_err();

    assert!(error.to_string().contains("不安全"));
    assert!(!root.join("preferences.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_profile_catalog_is_still_a_valid_backup_snapshot() {
    let root = test_root("empty-profiles");
    let source = root.join("source");
    let archive = root.join("backup.zip");
    fs::create_dir_all(&source).unwrap();

    let summary = BackupManager::new(&source).export_to(&archive).unwrap();
    let prepared = BackupManager::new(root.join("target"))
        .prepare_restore(&archive)
        .unwrap();

    assert_eq!(summary.file_count, 4);
    assert_eq!(prepared.file_count(), 4);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restores_legacy_v1_snapshot_with_an_empty_override_catalog() {
    let root = test_root("legacy-v1");
    let source = root.join("source");
    let archive = root.join("backup-v2.zip");
    let legacy = root.join("backup-v1.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17_890);
    BackupManager::new(&source).export_to(&archive).unwrap();
    make_legacy_v1_archive(&archive, &legacy);

    BackupManager::new(root.join("target"))
        .prepare_restore(&legacy)
        .unwrap()
        .activate()
        .unwrap()
        .commit()
        .unwrap();

    let overrides = YamlOverrideStore::new(root.join("target/yaml-overrides"))
        .unwrap()
        .load()
        .unwrap();
    assert!(overrides.items.is_empty());
    fs::remove_dir_all(root).unwrap();
}

fn create_snapshot(root: &Path, appearance: AppearancePreference, tray: bool, port: u16) {
    fs::create_dir_all(root).unwrap();
    AppPreferencesStore::new(root.join("preferences.json"))
        .save(&AppPreferences {
            appearance,
            traffic_tray_visible: tray,
            ..AppPreferences::default()
        })
        .unwrap();
    atomic_write(
        &root.join("controlled-config/override.yaml"),
        format!("mixed-port: {port}\n").as_bytes(),
    )
    .unwrap();
    let source = root.join("import.yaml");
    fs::write(&source, PROFILE).unwrap();
    let profiles = ProfileStore::new(root.join("profiles")).unwrap();
    let record = profiles.import_local(source).unwrap();
    profiles.activate(&record.id).unwrap();
    let override_source = root.join("managed-override.yaml");
    fs::write(&override_source, format!("mixed-port: {}\n", port + 100)).unwrap();
    YamlOverrideStore::new(root.join("yaml-overrides"))
        .unwrap()
        .import_paths([override_source])
        .unwrap();
}

fn read_authoritative_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut snapshot = Vec::new();
    for relative in [
        "preferences.json",
        "controlled-config/override.yaml",
        "profiles/profiles.json",
        "yaml-overrides/overrides.json",
    ] {
        snapshot.push((relative.into(), fs::read(root.join(relative)).unwrap()));
    }
    let mut profiles = fs::read_dir(root.join("profiles/files"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    profiles.sort();
    for path in profiles {
        snapshot.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            fs::read(path).unwrap(),
        ));
    }
    let mut overrides = fs::read_dir(root.join("yaml-overrides/files"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    overrides.sort();
    for path in overrides {
        snapshot.push((
            format!("override:{}", path.file_name().unwrap().to_string_lossy()),
            fs::read(path).unwrap(),
        ));
    }
    snapshot
}

fn archive_names(path: &Path) -> Vec<String> {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|index| zip.by_index(index).unwrap().name().to_owned())
        .collect()
}

fn rewrite_archive(source: &Path, destination: &Path, mutate: impl Fn(&str, &mut Vec<u8>)) {
    let mut reader = ZipArchive::new(fs::File::open(source).unwrap()).unwrap();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..reader.len() {
        let mut entry = reader.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        mutate(&name, &mut bytes);
        writer
            .start_file(&name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    fs::write(destination, writer.finish().unwrap().into_inner()).unwrap();
}

fn make_legacy_v1_archive(source: &Path, destination: &Path) {
    let mut reader = ZipArchive::new(fs::File::open(source).unwrap()).unwrap();
    let mut entries = Vec::new();
    for index in 0..reader.len() {
        let mut entry = reader.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if !name.starts_with("yaml-overrides/") {
            entries.push((name, bytes));
        }
    }
    let manifest = entries
        .iter_mut()
        .find(|(name, _)| name == MANIFEST_PATH)
        .unwrap();
    let mut decoded: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
    decoded["format_version"] = serde_json::json!(1);
    decoded["files"].as_array_mut().unwrap().retain(|file| {
        !file["path"]
            .as_str()
            .unwrap()
            .starts_with("yaml-overrides/")
    });
    manifest.1 = serde_json::to_vec_pretty(&decoded).unwrap();

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    fs::write(destination, writer.finish().unwrap().into_inner()).unwrap();
}

fn test_root(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "zenclash-backup-{name}-{}-{sequence}",
        std::process::id()
    ))
}
