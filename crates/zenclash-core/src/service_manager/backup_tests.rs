//! Ordinary child and loopback regressions; no native authorization or TUN effects.

use super::*;
use crate::{
    AppPreferences, AppPreferencesStore, BackupManager, MihomoClient, ProfileStore,
    YamlOverrideStore,
};
use std::{fs, time::Duration};

struct Fixture {
    child: crate::core_session::ownership_tests::ChildFixture,
    manager: ServiceManager,
    store: ControlledConfigStore,
    root: PathBuf,
    archive: PathBuf,
}

impl Fixture {
    async fn new(label: &str, tun: bool) -> Self {
        let child = crate::core_session::ownership_tests::ChildFixture::with_config_path(
            label,
            "home/target/controlled-config/local-runtime/slot0/runtime.yaml",
        )
        .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(child.process.clone()).unwrap(),
        )
        .unwrap();
        let home = child.process.launch_config().home_dir.clone();
        let root = home.join("target");
        let source = home.join("source");
        let archive = home.join("backup.zip");
        for (path, enabled) in [(&root, false), (&source, tun)] {
            fs::create_dir_all(path.join("controlled-config")).unwrap();
            AppPreferencesStore::new(path.join("preferences.json"))
                .save(&AppPreferences::default())
                .unwrap();
            fs::write(path.join("controlled-config/override.yaml"), "{}\n").unwrap();
            fs::write(
                path.join("input.yaml"),
                format!("tun: {{enable: {enabled}}}\nmode: direct\n"),
            )
            .unwrap();
            let profiles = ProfileStore::new(path.join("profiles")).unwrap();
            let profile = profiles.import_local(path.join("input.yaml")).unwrap();
            profiles.activate(&profile.id).unwrap();
            YamlOverrideStore::new(path.join("yaml-overrides")).unwrap();
        }
        let store = ControlledConfigStore::new(root.join("controlled-config"));
        fs::write(store.runtime_path(), "tun: {enable: false}\nproxy-providers:\n  held:\n    type: file\n    path: nodes.yaml\n").unwrap();
        fs::write(
            home.join("nodes.yaml"),
            "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
        )
        .unwrap();
        fs::write(home.join("cert.pem"), b"transaction held certificate").unwrap();
        BackupManager::new(source).export_to(&archive).unwrap();
        let manager = ServiceManager::new(
            session.clone(),
            crate::TrafficCaptureSession::new(session, store.clone(), None, None),
        );
        Self {
            child,
            manager,
            store,
            root,
            archive,
        }
    }

    fn prepare(&self) -> PreparedBackupRestore {
        BackupManager::new(&self.root)
            .prepare_restore(&self.archive)
            .unwrap()
    }
}

#[tokio::test]
async fn backup_authorization_wait_releases_live_data_and_preserves_local_child() {
    let fixture = Fixture::new("geodata-backup-authorization-wait", true).await;
    let pid = fixture.child.process.snapshot().pid;
    let cached = fixture.store.cached_runtime_payload().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let manager = fixture.manager.clone();
    let prepared = fixture.prepare();
    let pending = tokio::spawn(async move {
        manager
            .prepare_backup_restore_admitted(prepared, |_, request| async move {
                entered_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(request)
            })
            .await
    });
    entered_rx.await.unwrap();
    let path = fixture.root.join("preferences.json");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            AppPreferencesStore::new(path).save(&AppPreferences::default())
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(fixture.child.process.snapshot().pid, pid);
    assert_eq!(fixture.store.cached_runtime_payload().unwrap(), cached);
    assert_eq!(fixture.manager.session.generation(), 0);
    release_tx.send(()).unwrap();
    let (prepared, application) = pending.await.unwrap().unwrap();
    let application = application.unwrap();
    assert!(application.config.requires_service());
    assert!(!application.request.allow_authorization);
    drop(application);
    drop(prepared);
    fixture.manager.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_changed_runtime_cache_rejects_activation_before_disk_swap() {
    let fixture = Fixture::new("geodata-backup-stale-snapshot", false).await;
    let original_index = fs::read(fixture.root.join("profiles/profiles.json")).unwrap();
    let (prepared, _) = fixture
        .manager
        .prepare_backup_restore(fixture.prepare())
        .await
        .unwrap();
    fs::write(fixture.store.runtime_path(), "mode: global\n").unwrap();
    let session = fixture.manager.session.clone();
    assert!(
        tokio::task::spawn_blocking(move || prepared.activate_for_session(&session))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.root.join("profiles/profiles.json")).unwrap(),
        original_index
    );
    assert_eq!(
        fixture.store.cached_runtime_payload().unwrap().unwrap(),
        "mode: global\n"
    );
    assert_eq!(fixture.manager.session.generation(), 0);
    fixture.manager.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_changed_activated_candidate_is_rejected_before_local_reload() {
    let fixture = Fixture::new("geodata-backup-changed-candidate", false).await;
    let pid = fixture.child.process.snapshot().pid;
    let (prepared, application) = fixture
        .manager
        .prepare_backup_restore(fixture.prepare())
        .await
        .unwrap();
    let session = fixture.manager.session.clone();
    let transaction = tokio::task::spawn_blocking(move || prepared.activate_for_session(&session))
        .await
        .unwrap()
        .unwrap();
    let store = transaction
        .authorize_controlled_store(fixture.store.clone())
        .unwrap();
    let profile = transaction
        .profile_store()
        .unwrap()
        .active_path()
        .unwrap()
        .unwrap();
    fs::write(profile, "tun: {enable: true}\n").unwrap();
    assert!(
        fixture
            .manager
            .apply_backup_config(
                &store,
                &transaction.previous_runtime_snapshot().unwrap(),
                application.unwrap()
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.child.process.snapshot().pid, pid);
    assert_eq!(fixture.manager.session.generation(), 0);
    assert!(store.cached_runtime_payload().unwrap().is_none());
    transaction.rollback().unwrap();
    fixture.manager.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_local_apply_and_outer_rollback_recover_held_deleted_resources() {
    let fixture = Fixture::new("geodata-backup-transaction-recovery", false).await;
    let pid = fixture.child.process.snapshot().pid;
    let (prepared, application) = fixture
        .manager
        .prepare_backup_restore(fixture.prepare())
        .await
        .unwrap();
    let home = &fixture.child.process.launch_config().home_dir;
    fs::remove_file(home.join("nodes.yaml")).unwrap();
    fs::remove_file(home.join("cert.pem")).unwrap();
    let admission = fixture
        .manager
        .session
        .begin_backup_restore()
        .await
        .unwrap();
    let session = fixture.manager.session.clone();
    let mut transaction =
        tokio::task::spawn_blocking(move || prepared.activate_for_session(&session))
            .await
            .unwrap()
            .unwrap();
    let snapshot = transaction.previous_runtime_snapshot().unwrap();
    let store = transaction
        .authorize_controlled_store(fixture.store.clone())
        .unwrap();
    assert!(store.cached_runtime_payload().unwrap().is_none());
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .manager
            .apply_backup_config(&store, &snapshot, application.unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(outcome, ServiceConfigOutcome::Local(_)));
    assert_eq!(fixture.manager.session.generation(), 1);
    transaction.rollback_in_place().unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .manager
            .session
            .restore_backup_snapshot(&store, &snapshot, &admission),
    )
    .await
    .unwrap()
    .unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
    let provider: serde_yaml::Value = serde_yaml::from_slice(
        &fs::read(yaml["proxy-providers"]["held"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::read(provider["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"transaction held certificate"
    );
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(fixture.child.process.snapshot().pid, pid);
    assert_eq!(fixture.manager.session.generation(), 2);
    // A second rejected hot reload must not overwrite resources of the first.
    let accepted = store.cached_runtime_payload().unwrap().unwrap();
    let cert = PathBuf::from(provider["proxies"][0]["certificate"].as_str().unwrap());
    // Prepare another immutable resource revision using the original source names.
    fs::write(
        home.join("nodes.yaml"),
        "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
    )
    .unwrap();
    fs::write(home.join("cert.pem"), b"next rejected certificate").unwrap();
    let candidate = crate::backup::BackupRuntimeCandidate {
        data_root: fixture.root.clone(),
        profile: home.join("next.yaml"),
        overrides: vec![],
        payload: "tun: {enable: false}\n".into(),
        patch: b"{}\n".to_vec(),
    };
    // Use the accepted cache with original names solely to freeze the new snapshot.
    fs::write(
        store.runtime_path(),
        "tun: {enable: false}\nproxy-providers:\n  held:\n    type: file\n    path: nodes.yaml\n",
    )
    .unwrap();
    let (_, next_snapshot) = fixture
        .manager
        .session
        .prepare_service_backup_config(
            &store,
            (
                fixture
                    .manager
                    .session
                    .runtime_descriptor()
                    .binding_generation(),
                2,
            ),
            &candidate,
        )
        .await
        .unwrap();
    fs::write(store.runtime_path(), &accepted).unwrap();
    fixture.child.responder_for_test_abort();
    tokio::task::yield_now().await;
    let address = fixture
        .child
        .process
        .launch_config()
        .endpoint
        .controller
        .trim_start_matches("http://");
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let server_mode = mode.clone();
    let server_requests = requests.clone();
    let reject = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let read = stream.read(&mut bytes).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&bytes[..read]);
                assert!(request.len() < 64 * 1024);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            server_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response: &[u8] = match server_mode.load(std::sync::atomic::Ordering::SeqCst) {
                0 => b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                1 => continue,
                _ => b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            };
            stream.write_all(response).await.unwrap();
        }
    });
    assert!(
        fixture
            .manager
            .session
            .restore_backup_snapshot(&store, &next_snapshot, &admission)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&cert).unwrap(), b"transaction held certificate");
    assert_eq!(store.cached_runtime_payload().unwrap().unwrap(), accepted);
    assert_eq!(fixture.manager.session.generation(), 2);
    mode.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(
        fixture
            .manager
            .session
            .restore_backup_snapshot(&store, &next_snapshot, &admission)
            .await
            .is_err()
    );
    assert_eq!(fixture.manager.session.generation(), 3);
    let count = requests.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        fixture
            .manager
            .session
            .restore_backup_snapshot(&store, &snapshot, &admission)
            .await
            .is_err()
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), count);
    assert_eq!(fs::read(&cert).unwrap(), b"transaction held certificate");
    fs::remove_file(home.join("nodes.yaml")).unwrap();
    fs::remove_file(home.join("cert.pem")).unwrap();
    mode.store(2, std::sync::atomic::Ordering::SeqCst);
    drop(transaction);
    drop(admission);
    fixture
        .manager
        .session
        .retry_backup_restore()
        .await
        .unwrap();
    assert!(fixture.manager.session.pending_backup_restore().is_none());
    let recovered: serde_yaml::Value =
        serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
    let provider: serde_yaml::Value = serde_yaml::from_slice(
        &fs::read(
            recovered["proxy-providers"]["held"]["path"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::read(provider["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"next rejected certificate"
    );
    assert_eq!(fixture.child.process.snapshot().pid, pid);
    reject.abort();
    fixture.manager.session.shutdown().await.unwrap();
}
