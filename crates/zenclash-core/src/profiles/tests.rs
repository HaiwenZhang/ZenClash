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
fn legacy_profile_metadata_gets_safe_update_defaults() {
    let record: ProfileRecord = serde_json::from_value(serde_json::json!({
        "id": "legacy",
        "name": "Legacy",
        "file_name": "legacy.yaml",
        "source": {
            "kind": "remote",
            "url": "https://example.com/profile.yaml",
            "user_agent": "clash.meta"
        },
        "updated_at": 100,
        "size_bytes": 42
    }))
    .unwrap();

    assert!(!record.auto_update);
    assert_eq!(
        record.update_interval_minutes,
        DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES
    );
    assert!(record.subscription.usage.is_none());
    let ProfileSource::Remote { options, .. } = record.source else {
        panic!("legacy record must remain remote");
    };
    assert_eq!(options, RemoteProfileOptions::default());
    assert!(record.update_cron.is_none());
}

#[test]
fn due_profiles_respect_source_policy_and_interval() {
    let remote = ProfileRecord {
        id: "remote".into(),
        name: "Remote".into(),
        file_name: "remote.yaml".into(),
        source: ProfileSource::Remote {
            url: "https://example.com/profile.yaml".into(),
            user_agent: "clash.meta".into(),
            options: RemoteProfileOptions::default(),
        },
        updated_at: 1_000,
        size_bytes: 42,
        auto_update: true,
        update_interval_minutes: 60,
        update_cron: None,
        subscription: SubscriptionMetadata::default(),
    };
    let mut disabled = remote.clone();
    disabled.id = "disabled".into();
    disabled.auto_update = false;
    let mut local = remote.clone();
    local.id = "local".into();
    local.source = ProfileSource::Local {
        original_path: "/tmp/local.yaml".into(),
    };
    let catalog = ProfileCatalog {
        active: None,
        profiles: vec![remote, disabled, local],
    };

    assert!(catalog.due_profile_ids(4_599).is_empty());
    assert_eq!(catalog.due_profile_ids(4_600), vec!["remote"]);
}

#[test]
fn update_policy_is_persisted_only_for_remote_profiles() {
    let root = test_root("update-policy");
    fs::create_dir_all(&root).unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let remote = store
        .store_profile(
            "remote".into(),
            ProfileSource::Remote {
                url: "https://example.com/profile.yaml".into(),
                user_agent: "clash.meta".into(),
                options: RemoteProfileOptions::default(),
            },
            "mixed-port: 7890\n",
        )
        .unwrap();
    let source = root.join("local.yaml");
    fs::write(&source, "mixed-port: 7891\n").unwrap();
    let local = store.import_local(source).unwrap();

    store.set_update_policy(&remote.id, true, 60).unwrap();
    let catalog = store.load().unwrap();
    let persisted = catalog
        .profiles
        .iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();

    assert!(persisted.auto_update);
    assert_eq!(persisted.update_interval_minutes, 60);
    assert!(persisted.update_cron.is_none());
    assert!(store.set_update_policy(&local.id, true, 60).is_err());
    assert!(store
        .set_update_policy(&remote.id, true, MIN_PROFILE_UPDATE_INTERVAL_MINUTES - 1)
        .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn five_field_cron_drives_due_checks_and_request_settings_persist_atomically() {
    assert!(!schedule::cron_update_due("* * * * *", 120, 179).unwrap());
    assert!(schedule::cron_update_due("* * * * *", 120, 180).unwrap());
    assert!(schedule::cron_update_due("0 * * * * *", 120, 180).is_err());

    let root = test_root("profile-cron-settings");
    let store = ProfileStore::new(root.join("store")).unwrap();
    let remote = store
        .store_profile(
            "cron".into(),
            ProfileSource::Remote {
                url: "https://example.com/profile.yaml".into(),
                user_agent: "clash.meta".into(),
                options: RemoteProfileOptions::default(),
            },
            "mixed-port: 7890\n",
        )
        .unwrap();
    let options = RemoteProfileOptions::new("Bearer cron-secret", true)
        .unwrap()
        .with_download_policy(45, true)
        .unwrap();
    assert!(RemoteProfileOptions::default()
        .with_download_policy(0, false)
        .is_err());

    store
        .set_remote_request_settings(
            &remote.id,
            "Cron renamed",
            "https://example.net/updated.yaml",
            "ZenClash-Cron",
            options.clone(),
            Some("*/5 * * * *".into()),
        )
        .unwrap();
    let saved = store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();

    assert!(saved.auto_update);
    assert_eq!(saved.name, "Cron renamed");
    assert_eq!(saved.update_cron.as_deref(), Some("*/5 * * * *"));
    let mut scheduled = saved.clone();
    scheduled.updated_at = 120;
    let catalog = ProfileCatalog {
        active: None,
        profiles: vec![scheduled],
    };
    assert!(catalog.due_profile_ids(299).is_empty());
    assert_eq!(catalog.due_profile_ids(300), vec![remote.id.clone()]);
    let ProfileSource::Remote {
        url,
        user_agent,
        options: saved_options,
    } = saved.source
    else {
        panic!("expected remote profile");
    };
    assert_eq!(url, "https://example.net/updated.yaml");
    assert_eq!(user_agent, "ZenClash-Cron");
    assert_eq!(saved_options, options);

    let before_rejected_edit = store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert!(store
        .set_remote_request_settings(
            &remote.id,
            "must not persist",
            "https://user:secret@example.net/profile.yaml",
            "must-not-persist",
            RemoteProfileOptions::default(),
            Some("0 * * * * *".into()),
        )
        .is_err());
    let after_rejected_edit = store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert_eq!(after_rejected_edit, before_rejected_edit);

    store.set_update_policy(&remote.id, true, 360).unwrap();
    let saved = store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert!(saved.update_cron.is_none());
    assert_eq!(saved.update_interval_minutes, 360);
    fs::remove_dir_all(root).unwrap();
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

    let error = download_profile(
        &format!("http://{address}/profile.yaml"),
        "clash.meta",
        &RemoteProfileOptions::default(),
        None,
    )
    .await
    .unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("超过 16 MiB"));
}

#[tokio::test]
async fn remote_profile_uses_authorized_http_proxy_and_persists_response_metadata() {
    let root = test_root("authorized-proxy-subscription");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        assert!(request.starts_with("GET http://subscription.invalid/profile.yaml HTTP/1.1"));
        assert!(request.contains("authorization: Bearer integration-secret\r\n"));
        assert!(request.contains("user-agent: ZenClash-Test\r\n"));
        let payload = "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n";
        write!(
            stream,
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/yaml\r\n",
                "Subscription-Userinfo: upload=10; download=20; total=1000; expire=2000000000\r\n",
                "Profile-Web-Page-Url: https://example.com/account\r\n",
                "Profile-Update-Interval: 6\r\n",
                "Content-Length: {}\r\n",
                "Connection: close\r\n\r\n{}"
            ),
            payload.len(),
            payload
        )
        .unwrap();
    });
    let store = ProfileStore::new(root.join("store")).unwrap();
    let options = RemoteProfileOptions::new("Bearer integration-secret", true)
        .unwrap()
        .with_download_policy(45, true)
        .unwrap();

    let record = store
        .add_remote_with_options(
            "authorized",
            "http://subscription.invalid/profile.yaml",
            "ZenClash-Test",
            options.clone(),
            Some(address.port()),
        )
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(
        record.update_interval_minutes,
        DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES
    );
    assert_eq!(
        record.subscription.suggested_update_interval_minutes,
        Some(360)
    );
    assert_eq!(record.subscription.usage.as_ref().unwrap().used(), 30);
    assert_eq!(record.subscription.usage.as_ref().unwrap().total, 1_000);
    assert_eq!(
        record.subscription.home_url.as_deref(),
        Some("https://example.com/account")
    );
    let ProfileSource::Remote {
        options: stored_options,
        ..
    } = &record.source
    else {
        panic!("expected remote profile");
    };
    assert_eq!(stored_options, &options);
    assert!(!format!("{record:?}").contains("integration-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(store.root().join("profiles.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn remote_profile_retries_a_failed_direct_request_through_mihomo_proxy() {
    let root = test_root("fallback-proxy-subscription");
    let unavailable = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let unavailable_address = unavailable.local_addr().unwrap();
    drop(unavailable);

    let proxy = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let expected_url = format!("http://{unavailable_address}/profile.yaml");
    let expected_request_target = expected_url.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = proxy.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        assert!(request.starts_with(&format!("GET {expected_request_target} HTTP/1.1")));
        assert!(request.contains("authorization: Bearer fallback-secret\r\n"));
        let payload = "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
        .unwrap();
    });
    let options = RemoteProfileOptions::new("Bearer fallback-secret", false).unwrap();
    assert_eq!(
        options.route(),
        RemoteProfileRoute::DirectWithMihomoFallback
    );
    let store = ProfileStore::new(root.join("store")).unwrap();

    let record = store
        .add_remote_with_options(
            "fallback",
            expected_url,
            "ZenClash-Fallback-Test",
            options,
            Some(proxy_address.port()),
        )
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(record.name, "fallback");
    assert_eq!(
        store.remote_route(&record.id).await.unwrap(),
        RemoteProfileRoute::DirectWithMihomoFallback
    );
    fs::remove_dir_all(root).unwrap();
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
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
fn profile_editor_replaces_and_rolls_back_without_overwriting_a_newer_payload() {
    let root = test_root("profile-editor-transaction");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    let original = "mixed-port: 7890\nmode: rule\n";
    let candidate = "mixed-port: 7891\nmode: global\n";
    fs::write(&source, original).unwrap();
    let store = ProfileStore::new(root.join("store")).unwrap();
    let profile = store.import_local(source).unwrap();
    let path = store.profile_path(&profile);

    let update = store
        .replace_payload(&profile.id, original, candidate)
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), candidate);
    assert_eq!(update.record.size_bytes, candidate.len() as u64);

    store.rollback_update(update).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(store
        .replace_payload(&profile.id, candidate, "mixed-port: 9000\n")
        .is_err());
    assert!(store
        .replace_payload(&profile.id, original, "ordinary: yaml\n")
        .is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), original);
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
                options: RemoteProfileOptions::default(),
            },
            "mixed-port: 7890\n",
        )
        .unwrap();
    let current = store
        .persist_remote_update(
            &original.id,
            &original,
            b"mixed-port: 17890\n".to_vec(),
            SubscriptionMetadata::default(),
        )
        .unwrap()
        .record;

    let error = store
        .persist_remote_update(
            &original.id,
            &original,
            b"mixed-port: 27890\n".to_vec(),
            SubscriptionMetadata::default(),
        )
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
