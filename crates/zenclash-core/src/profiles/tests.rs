use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

use super::*;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zenclash-profile-test-{name}-{}-{}",
        std::process::id(),
        unix_timestamp()
    ))
}

#[test]
fn imports_activates_and_persists_local_profile() {
    let root = test_root("local");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        &source,
        "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n",
    )
    .unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let profile = store.import_local(&source).unwrap();
    let active = store.activate(&profile.id).unwrap();

    assert!(active.is_file());
    assert_eq!(
        store.load().unwrap().active.as_deref(),
        Some(profile.id.as_str())
    );
    assert_eq!(store.active_path().unwrap(), Some(active));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_non_clash_yaml() {
    let error = validate_clash_yaml("name: ordinary yaml\n").unwrap_err();
    assert!(error.to_string().contains("Clash/Mihomo"));
}

#[test]
fn import_local_rejects_non_utf8_payload() {
    let root = test_root("invalid-utf8");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, [0xff, 0xfe, 0xfd]).unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();

    let error = store.import_local(source).unwrap_err();

    assert!(matches!(error, ProfileStoreError::InvalidYaml(_)));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn download_profile_rejects_oversized_content_length() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4_096];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_PROFILE_BYTES + 1
        )
        .unwrap();
    });

    let error = download_profile(&format!("http://{address}/profile.yaml"), "clash.meta")
        .await
        .unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("超过 16 MiB"));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_update_preserves_profile_imported_while_downloading() {
    let root = test_root("concurrent-update");
    let source = root.join("local.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "mixed-port: 7891\n").unwrap();

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (update_started_tx, update_started_rx) = mpsc::channel();
    let (finish_update_tx, finish_update_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        for request_index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let _ = stream.read(&mut request).unwrap();
            if request_index == 1 {
                update_started_tx.send(()).unwrap();
                finish_update_rx.recv().unwrap();
            }
            let payload = format!("mixed-port: {}\n", 7890 + request_index);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            )
            .unwrap();
        }
    });

    let store = ProfileStore::new(root.join("store")).unwrap();
    let remote = store
        .add_remote("remote", format!("http://{address}/profile.yaml"), "")
        .await
        .unwrap();
    let update_store = store.clone();
    let update_id = remote.id.clone();
    let update = tokio::spawn(async move { update_store.update_remote(&update_id).await });
    update_started_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap();

    let local = store.import_local(source).unwrap();
    finish_update_tx.send(()).unwrap();
    update.await.unwrap().unwrap();
    server.join().unwrap();

    let catalog = store.load().unwrap();
    assert!(catalog
        .profiles
        .iter()
        .any(|profile| profile.id == remote.id));
    assert!(catalog
        .profiles
        .iter()
        .any(|profile| profile.id == local.id));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn refuses_to_delete_active_profile() {
    let root = test_root("active-delete");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "mixed-port: 7890\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let profile = store.import_local(source).unwrap();
    store.activate(&profile.id).unwrap();

    assert!(matches!(
        store.delete(&profile.id),
        Err(ProfileStoreError::ActiveProfile)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delete_removes_non_active_profile_from_disk_and_catalog() {
    let root = test_root("delete");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "mixed-port: 7890\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let profile = store.import_local(source).unwrap();
    let profile_path = store.profile_path(&profile);

    store.delete(&profile.id).unwrap();

    assert_eq!(
        (profile_path.exists(), store.load().unwrap().profiles),
        (false, Vec::new())
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_write_replaces_existing_file() {
    let root = test_root("atomic-replace");
    let path = root.join("profiles.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(&path, b"old").unwrap();

    atomic_write(&path, b"new").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"new");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn load_rejects_an_oversized_profile_index() {
    let root = test_root("oversized-index");
    let store = ProfileStore::new(&root).unwrap();
    fs::write(
        root.join("profiles.json"),
        vec![b' '; MAX_PROFILE_INDEX_BYTES + 1],
    )
    .unwrap();

    let error = store.load().unwrap_err();

    assert!(matches!(
        error,
        ProfileStoreError::IndexTooLarge { limit_mib: 4 }
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_update_restores_previous_profile_payload() {
    let root = test_root("rollback-update");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "mixed-port: 7890\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let previous_record = store.import_local(&source).unwrap();
    let path = store.profile_path(&previous_record);
    let previous_payload = fs::read(&path).unwrap();
    atomic_write(&path, b"mixed-port: 9999\n").unwrap();
    let record = previous_record.clone();

    store
        .rollback_update(ProfileUpdate {
            record,
            previous_record,
            previous_payload,
            applied_payload: b"mixed-port: 9999\n".to_vec(),
        })
        .unwrap();

    assert_eq!(fs::read_to_string(path).unwrap(), "mixed-port: 7890\n");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_activation_restores_previous_active_profile() {
    let root = test_root("rollback-activation");
    fs::create_dir_all(&root).unwrap();
    let first_path = root.join("first.yaml");
    let second_path = root.join("second.yaml");
    fs::write(&first_path, "mixed-port: 7890\n").unwrap();
    fs::write(&second_path, "mixed-port: 7891\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let first = store.import_local(first_path).unwrap();
    let second = store.import_local(second_path).unwrap();
    store.activate(&first.id).unwrap();
    let activation = store.activate_reversible(&second.id).unwrap();

    store.rollback_activation(activation).unwrap();

    assert_eq!(store.load().unwrap().active, Some(first.id));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_activation_refuses_to_overwrite_newer_selection() {
    let root = test_root("stale-activation");
    fs::create_dir_all(&root).unwrap();
    let first_path = root.join("first.yaml");
    let second_path = root.join("second.yaml");
    fs::write(&first_path, "mixed-port: 7890\n").unwrap();
    fs::write(&second_path, "mixed-port: 7891\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let first = store.import_local(first_path).unwrap();
    let second = store.import_local(second_path).unwrap();
    let stale = store.activate_reversible(&second.id).unwrap();
    store.activate(&first.id).unwrap();

    let error = store.rollback_activation(stale).unwrap_err();

    assert!(matches!(error, ProfileStoreError::Transaction(_)));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_update_refuses_to_overwrite_newer_profile_payload() {
    let root = test_root("stale-update");
    fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    fs::write(&source, "mixed-port: 7890\n").unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let previous_record = store.import_local(source).unwrap();
    let path = store.profile_path(&previous_record);
    let previous_payload = fs::read(&path).unwrap();
    atomic_write(&path, b"mixed-port: 9999\n").unwrap();
    let record = previous_record.clone();

    let error = store
        .rollback_update(ProfileUpdate {
            record,
            previous_record,
            previous_payload,
            applied_payload: b"mixed-port: 8888\n".to_vec(),
        })
        .unwrap_err();

    assert_eq!(
        (
            matches!(error, ProfileStoreError::Transaction(_)),
            fs::read_to_string(path).unwrap()
        ),
        (true, "mixed-port: 9999\n".to_owned())
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_update_refuses_to_overwrite_a_profile_changed_during_download() {
    let root = test_root("stale-remote-download");
    let store = ProfileStore::new(root.join("store")).unwrap();
    let original = store
        .store_profile(
            "remote".into(),
            ProfileSource::Remote {
                url: "https://old.example/profile".into(),
                user_agent: "clash.meta".into(),
            },
            "mixed-port: 7890\n",
        )
        .unwrap();
    let current = store
        .persist_remote_update(&original.id, &original, b"mixed-port: 17890\n".to_vec())
        .unwrap()
        .record;

    let error = store
        .persist_remote_update(&original.id, &original, b"mixed-port: 27890\n".to_vec())
        .unwrap_err();

    assert_eq!(
        (
            matches!(error, ProfileStoreError::Transaction(_)),
            fs::read_to_string(store.profile_path(&current)).unwrap(),
        ),
        (true, "mixed-port: 17890\n".into())
    );
    fs::remove_dir_all(root).unwrap();
}
