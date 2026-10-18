use std::{io::Write, net::TcpListener, thread};

use crate::MihomoEndpoint;

use super::test_support::{read_controller_request, read_controller_request_blocking};
use super::*;

#[tokio::test]
async fn queued_mode_rejects_a_binding_switched_while_waiting_for_data() {
    queued_mode_rejects_changed_binding(false).await;
}

#[tokio::test]
async fn queued_mode_rejects_a_binding_switched_while_waiting_for_transition() {
    queued_mode_rejects_changed_binding(true).await;
}

async fn queued_mode_rejects_changed_binding(wait_for_transition: bool) {
    use std::{future::Future, task::Context};
    use tokio::io::AsyncWriteExt;

    let root = session_test_root("mode-binding-admission");
    std::fs::create_dir_all(&root).unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let client = MihomoClient::new(MihomoEndpoint::default())
        .unwrap()
        .with_config_validator(crate::CoreConfigValidator::new(
            CoreKind::Mihomo,
            root.join("unused-kernel"),
            root.join("home"),
        ))
        .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let lease = if wait_for_transition {
        DataWriteLease::shared([root.clone()]).unwrap()
    } else {
        DataWriteLease::exclusive([root.clone()])
    };
    let store = if wait_for_transition {
        store.with_write_lease(&lease)
    } else {
        store
    };
    let transition = session.transition.lock().await;
    let mut operation = Box::pin(session.set_mode(&store, "global"));
    assert!(
        operation
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(300), listener.accept()).await
        else {
            return false;
        };
        let request = read_controller_request(&mut stream).await.unwrap();
        assert!(!request.headers().is_empty());
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        true
    });
    client
        .switch_to_direct(MihomoEndpoint::new(format!("http://{address}"), ""))
        .await
        .unwrap();
    drop(transition);
    drop(lease);
    assert!(matches!(
        operation.await,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
    assert!(
        !server.await.unwrap(),
        "stale operation reached the replacement controller"
    );
    assert_eq!(session.generation(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ordinary_public_snapshot_cannot_consume_pending_backup_without_admission() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("backup-ordinary-no-authority");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store.materialize(&source).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut payloads = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_controller_request(&mut stream).await.unwrap();
            let headers = request.headers();
            let body = request.body();
            assert!(headers.starts_with("PUT /configs"));
            payloads.push(
                serde_json::from_slice::<serde_json::Value>(body).unwrap()["payload"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            if index == 1 {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
        payloads
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(source.clone()),
        vec![],
    )
    .unwrap();
    let snapshot = session.capture_restore_snapshot(&store).unwrap();
    let admission = session.begin_backup_restore().await.unwrap();
    assert!(
        session
            .restore_backup_snapshot(&store, &snapshot, &admission)
            .await
            .is_err()
    );
    assert_eq!(session.pending_backup_restore(), Some(session.generation()));
    drop(admission);
    let ordinary = session.restore_snapshot(&store, &snapshot).await;
    assert!(
        ordinary.is_err(),
        "public ordinary restore consumed protected pending backup recovery"
    );
    assert_eq!(session.pending_backup_restore(), Some(session.generation()));
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(store.runtime_path()).unwrap();
    let outcome = session.retry_backup_restore().await.unwrap();
    assert_eq!(outcome.generation, session.generation());
    assert!(session.pending_backup_restore().is_none());
    let payloads = server.await.unwrap();
    assert_eq!(payloads[0], payloads[1]);
    assert!(payloads[1].contains("8011"));
    session.next_generation();
    assert!(session.pending_backup_restore().is_none());
    session.shutdown().await.unwrap();
    assert!(matches!(
        session.begin_backup_restore().await,
        Err(CoreSessionError::ShuttingDown)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_backup_restore_retains_exact_snapshot_for_retry_without_sources() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("backup-retry-held-snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mixed-port: 8011\nmode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store.materialize(&source).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut payloads = Vec::new();
        for index in 0..6 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_controller_request(&mut stream).await.unwrap();
            let headers = request.headers();
            let body = request.body();
            if matches!(index, 2 | 3) {
                assert!(headers.starts_with("PATCH /configs"));
            } else if matches!(index, 1 | 4) {
                assert!(headers.starts_with("GET /configs"));
            } else {
                payloads.push(
                    serde_json::from_slice::<serde_json::Value>(body).unwrap()["payload"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            if matches!(index, 1 | 4) {
                let body = r#"{"mode":"rule"}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            } else if matches!(index, 3 | 5) {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
        payloads
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(source.clone()),
        vec![],
    )
    .unwrap();
    let snapshot = session.capture_restore_snapshot(&store).unwrap();
    let admission = session.begin_backup_restore().await.unwrap();
    assert!(
        session
            .restore_backup_snapshot(&store, &snapshot, &admission)
            .await
            .is_err()
    );
    assert_eq!(session.pending_backup_restore(), Some(session.generation()));
    drop(admission);
    assert!(session.set_mode(&store, "global").await.is_err());
    assert_eq!(
        session.pending_backup_restore(),
        Some(session.generation()),
        "failed mode request discarded the only exact backup snapshot"
    );
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(store.runtime_path()).unwrap();
    let outcome = session.retry_backup_restore().await.unwrap();
    assert_eq!(outcome.generation, session.generation());
    assert!(session.pending_backup_restore().is_none());
    let payloads = server.await.unwrap();
    assert_eq!(payloads[0], payloads[1]);
    assert!(payloads[1].contains("8011"));
    session.next_generation();
    assert!(session.pending_backup_restore().is_none());
    session.shutdown().await.unwrap();
    assert!(matches!(
        session.begin_backup_restore().await,
        Err(CoreSessionError::ShuttingDown)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn successful_partial_mode_is_merged_into_pending_backup_retry_target() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("backup-retry-saved-mode-delta");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store.materialize(&source).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut payloads = Vec::new();
        for index in 0..5 {
            let (mut stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let request = read_controller_request(&mut stream).await.unwrap();
            let headers = request.headers();
            let body = request.body();
            if matches!(index, 0 | 4) {
                assert!(headers.starts_with("PUT /configs"));
                let body: serde_json::Value = serde_json::from_slice(body).unwrap();
                payloads.push(body["payload"].as_str().unwrap().to_owned());
            } else if index == 2 {
                assert!(headers.starts_with("PATCH /configs"));
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(body).unwrap(),
                    serde_json::json!({"mode":"global"})
                );
            } else {
                assert!(headers.starts_with("GET /configs"));
            }
            if matches!(index, 1 | 3) {
                let mode = if index == 1 { "rule" } else { "global" };
                let body = format!(r#"{{"mode":"{mode}"}}"#);
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            } else if index != 0 {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
        payloads
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(source.clone()),
        vec![],
    )
    .unwrap();
    let snapshot = session.capture_restore_snapshot(&store).unwrap();
    let admission = session.begin_backup_restore().await.unwrap();
    assert!(
        session
            .restore_backup_snapshot(&store, &snapshot, &admission)
            .await
            .is_err()
    );
    drop(admission);
    session.set_mode(&store, "global").await.unwrap();
    assert_eq!(session.pending_backup_restore(), Some(session.generation()));
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(store.runtime_path()).unwrap();
    session.retry_backup_restore().await.unwrap();
    let payloads = server.await.unwrap();
    let recovered: serde_yaml::Value = serde_yaml::from_str(&payloads[1]).unwrap();
    assert_eq!(
        recovered["mode"].as_str(),
        Some("global"),
        "backup retry must preserve a later durably accepted mode delta"
    );
    assert_eq!(recovered["rules"][0].as_str(), Some("MATCH,DIRECT"));
    assert!(session.pending_backup_restore().is_none());
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_mode_uses_the_profile_committed_by_the_preceding_transition() {
    queued_update_uses_committed_config(QueuedConfigUpdate::Mode).await;
}

#[tokio::test]
async fn queued_patch_uses_the_profile_and_override_chain_committed_by_the_preceding_transition() {
    queued_update_uses_committed_config(QueuedConfigUpdate::Patch).await;
}

#[tokio::test]
async fn queued_override_reapply_does_not_reactivate_the_previously_displayed_profile() {
    queued_update_uses_committed_config(QueuedConfigUpdate::Reapply).await;
}

#[tokio::test]
async fn reapply_without_a_committed_profile_does_not_mutate_saved_configuration() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-reapply-uncommitted-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let yaml_override = root.join("saved.yaml");
    std::fs::write(&yaml_override, "allow-lan: true\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        session
            .apply(
                &store,
                EffectiveConfigIntent::ReapplyCurrent {
                    overrides: vec![yaml_override.clone()]
                }
            )
            .await,
        Err(CoreSessionError::NoCommittedProfile)
    ));
    assert_eq!(
        std::fs::read_to_string(yaml_override).unwrap(),
        "allow-lan: true\n"
    );
    assert!(!store.runtime_path().exists());
    assert_eq!(session.generation(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn committed_profile_snapshot_does_not_read_disk_or_wait_for_an_active_transition() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-committed-snapshot-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mode: rule\n").unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
        Some(source.clone()),
        vec![],
    )
    .unwrap();
    std::fs::remove_file(&source).unwrap();
    let _active_transition = session.transition.lock().await;
    assert_eq!(
        session.committed_profile_snapshot(),
        CoreCommittedProfileSnapshot {
            profile_path: Some(source),
            generation: 0,
        }
    );
    std::fs::remove_dir(root).unwrap();
}

#[tokio::test]
async fn rolling_back_an_accepted_profile_invalidates_reads_of_the_temporary_runtime() {
    let _ports = fixed_listener_ports_guard().await;
    let root = std::env::temp_dir().join(format!(
        "zenclash-profile-rollback-generation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let previous = root.join("a.yaml");
    let candidate = root.join("b.yaml");
    std::fs::write(&previous, "mixed-port: 8011\nmode: rule\n").unwrap();
    std::fs::write(&candidate, "mixed-port: 8012\nmode: rule\n").unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            requests.push(String::from_utf8(request).unwrap());
            if index == 1 {
                let body = r#"{"mixed-port":8012,"mode":"rule"}"#;
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            } else {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
        }
        requests
    });
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&previous, &[], CoreKind::Mihomo)
        .unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(previous.clone()),
        vec![],
    )
    .unwrap();
    let transaction = session
        .stage_profile_application(&store, candidate, Some(previous.clone()), vec![], true)
        .await
        .unwrap();
    let read_generation = session.generation();
    let observed_runtime = session.client.runtime_config().await.unwrap();
    match transaction.rollback().await {
        CoreProfileRollbackOutcome::Runtime { result, .. } => result.unwrap(),
        CoreProfileRollbackOutcome::Validated => panic!("runtime transaction was not applied"),
    }
    let requests = server.join().unwrap();
    let restored: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(requests[0].starts_with("PUT /configs?force=true "));
    assert!(requests[0].contains("8012"));
    assert!(requests[1].starts_with("GET /configs "));
    assert!(requests[2].starts_with("PUT /configs?force=true "));
    assert!(requests[2].contains("8011"));
    assert_eq!(observed_runtime.mixed_port, 8012);
    assert_eq!(restored["mixed-port"].as_u64(), Some(8011));
    assert_eq!(
        session.committed_profile_snapshot().profile_path,
        Some(previous)
    );
    assert!(
        session.generation() > read_generation,
        "a read of the accepted candidate remained current after runtime rollback"
    );
}

#[tokio::test]
async fn restore_snapshot_replays_unnormalized_applied_bytes_after_sources_are_deleted() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("raw-runtime-snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let previous = root.join("previous.yaml");
    let overlay = root.join("overlay.yaml");
    let candidate = root.join("candidate.yaml");
    std::fs::write(&previous, "mixed-port: 8011\nmode: rule\n").unwrap();
    std::fs::write(&overlay, "allow-lan: true\n").unwrap();
    std::fs::write(&candidate, "mixed-port: 8012\nmode: global\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides(&previous, std::slice::from_ref(&overlay))
        .unwrap();
    let applied_bytes = std::fs::read_to_string(store.runtime_path()).unwrap();
    assert!(!applied_bytes.contains("geox-url"));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut payloads = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_controller_request_blocking(&mut stream).unwrap();
            assert!(request.headers().starts_with("PUT /configs?force=true "));
            payloads.push(
                serde_json::from_slice::<serde_json::Value>(request.body()).unwrap()["payload"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
        payloads
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(previous.clone()),
        vec![overlay.clone()],
    )
    .unwrap();
    let exclusive = DataWriteLease::exclusive(vec![root.clone()]);
    let authorized = store.with_write_lease(&exclusive);
    let snapshot = session.capture_restore_snapshot(&authorized).unwrap();
    assert_eq!(
        session
            .apply(
                &authorized,
                EffectiveConfigIntent::ActivateProfile {
                    profile: candidate,
                    overrides: vec![],
                }
            )
            .await
            .unwrap()
            .generation,
        1
    );
    std::fs::remove_file(&previous).unwrap();
    std::fs::remove_file(&overlay).unwrap();
    assert_eq!(
        session
            .restore_snapshot(&authorized, &snapshot)
            .await
            .unwrap()
            .generation,
        2
    );
    let payloads = server.join().unwrap();
    assert!(payloads[0].contains("8012"));
    assert_eq!(
        payloads[1], applied_bytes,
        "restore normalized or regenerated the accepted payload"
    );
    assert_eq!(
        std::fs::read_to_string(store.runtime_path()).unwrap(),
        applied_bytes
    );
    assert_eq!(
        session.committed_profile_snapshot().profile_path,
        Some(previous)
    );
    assert_eq!(session.transition.lock().await.overrides, vec![overlay]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn mode_preflight_read_failure_preserves_runtime_version_and_startup_cache() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("mode-preflight-version");
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("source.yaml");
    std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    let previous_cache = std::fs::read(store.runtime_path()).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        String::from_utf8(request).unwrap()
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(profile),
        vec![],
    )
    .unwrap();
    assert!(session.set_mode(&store, "global").await.is_err());
    assert!(server.join().unwrap().starts_with("GET /configs "));
    assert_eq!(
        session.generation(),
        0,
        "a failed preflight query must not invalidate runtime reads"
    );
    assert_eq!(std::fs::read(store.runtime_path()).unwrap(), previous_cache);
    assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_patch_does_not_invalidate_the_committed_runtime() {
    let _ports = fixed_listener_ports_guard().await;
    let root = session_test_root("invalid-patch-version");
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("source.yaml");
    std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    let previous_cache = std::fs::read(store.runtime_path()).unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
        Some(profile.clone()),
        vec![],
    )
    .unwrap();
    assert!(matches!(
        session
            .apply(
                &store,
                EffectiveConfigIntent::Patch {
                    profile,
                    patch: serde_json::json!([]),
                    overrides: vec![],
                }
            )
            .await,
        Err(CoreSessionError::Config(ControlledConfigError::NotMapping))
    ));
    assert_eq!(session.generation(), 0);
    assert_eq!(std::fs::read(store.runtime_path()).unwrap(), previous_cache);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unreadable_applied_cache_is_rejected_before_runtime_application() {
    let root = session_test_root("invalid-cache-version");
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("source.yaml");
    std::fs::write(&profile, "mode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(store.runtime_path(), [0xff_u8]).unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
        Some(profile.clone()),
        vec![],
    )
    .unwrap();
    assert!(matches!(
        session
            .stage_profile_application(&store, profile.clone(), Some(profile), vec![], true)
            .await,
        Err(CoreProfileStageError::Rejected(CoreSessionError::Config(
            ControlledConfigError::Io(_)
        )))
    ));
    assert_eq!(session.generation(), 0);
    assert_eq!(std::fs::read(store.runtime_path()).unwrap(), vec![0xff]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn direct_mode_rollback_invalidates_reads_of_the_temporary_runtime() {
    direct_rollback_invalidates_runtime_reads(true, false, false).await;
}

#[tokio::test]
async fn direct_patch_rollback_invalidates_reads_of_the_temporary_runtime() {
    direct_rollback_invalidates_runtime_reads(false, false, false).await;
}

#[tokio::test]
async fn direct_patch_rollback_restores_the_applied_cache_after_sources_and_overrides_change() {
    direct_rollback_invalidates_runtime_reads(false, true, false).await;
}

#[tokio::test]
async fn direct_patch_without_an_applied_cache_reports_unknown_after_persistence_failure() {
    direct_rollback_invalidates_runtime_reads(false, false, true).await;
}

async fn direct_rollback_invalidates_runtime_reads(
    mode_update: bool,
    changed_sources: bool,
    missing_cache: bool,
) {
    let _ports = fixed_listener_ports_guard().await;
    use tokio::io::AsyncWriteExt;

    let root = session_test_root(if mode_update {
        "direct-mode-rollback"
    } else {
        "direct-patch-rollback"
    });
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("source.yaml");
    std::fs::write(&profile, "mixed-port: 8011\nmode: rule\n").unwrap();
    let overlay = root.join("override.yaml");
    std::fs::write(&overlay, "allow-lan: true\n").unwrap();
    let overrides = if changed_sources {
        vec![overlay.clone()]
    } else {
        vec![]
    };
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&profile, &overrides, CoreKind::Mihomo)
        .unwrap();
    let applied_payload = std::fs::read_to_string(store.runtime_path()).unwrap();
    if missing_cache {
        std::fs::remove_file(store.runtime_path()).unwrap();
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(tokio::sync::Mutex::new((8011_u16, "rule".to_owned())));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(parking_lot::Mutex::new(Some(entered_tx)));
    let release = Arc::new(tokio::sync::Notify::new());
    let payloads = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let server_payloads = payloads.clone();
    let server_state = state.clone();
    let server_release = release.clone();
    let server = tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..if mode_update {
            6
        } else if missing_cache {
            2
        } else {
            3
        } {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let state = server_state.clone();
            let payloads = server_payloads.clone();
            let release = server_release.clone();
            let entered = entered.clone();
            workers.spawn(async move {
                let request = read_controller_request(&mut stream).await.unwrap();
let header_end = request.headers().len();
let request = request.into_bytes();
                let mutation = !request.starts_with(b"GET ");
                let body = if mutation {
                    let patch: serde_json::Value = serde_json::from_slice(&request[header_end + 4..]).unwrap();
                    let mut current = state.lock().await;
                    if mode_update {
                        current.1 = patch["mode"].as_str().unwrap().to_owned();
                    } else {
                        payloads.lock().push(patch["payload"].as_str().unwrap().to_owned());
                        let payload: serde_yaml::Value = serde_yaml::from_str(patch["payload"].as_str().unwrap()).unwrap();
                        current.0 = payload["mixed-port"].as_u64().unwrap() as u16;
                    }
                    String::new()
                } else {
                    let current = state.lock().await;
                    serde_json::json!({"mixed-port": current.0, "mode": current.1}).to_string()
                };
                if mutation {
                    let first = entered.lock().take();
                    if let Some(entered) = first {
                        entered.send(()).unwrap();
                        tokio::time::timeout(Duration::from_secs(5), release.notified()).await.unwrap();
                    }
                }
                let status = if mutation { "204 No Content" } else { "200 OK" };
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
        }
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    });
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        Some(profile.clone()),
        overrides,
    )
    .unwrap();
    if changed_sources {
        std::fs::write(&profile, "mixed-port: 7998\nmode: rule\n").unwrap();
        std::fs::write(&overlay, "allow-lan: false\n").unwrap();
    }
    let mutation_session = session.clone();
    let mutation_store = store.clone();
    let mutation_profile = profile.clone();
    let mutation = tokio::spawn(async move {
        if mode_update {
            mutation_session
                .set_mode(&mutation_store, "global")
                .await
                .map(|_| ())
        } else {
            mutation_session
                .apply(
                    &mutation_store,
                    EffectiveConfigIntent::Patch {
                        profile: mutation_profile,
                        patch: serde_json::json!({"mixed-port":8012}),
                        overrides: vec![],
                    },
                )
                .await
                .map(|_| ())
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let read_generation = session.generation();
    let temporary = session.client.runtime_config().await.unwrap();
    let concurrent = store
        .prepare_json_update(&profile, &serde_json::json!({"allow-lan":true}))
        .unwrap();
    store.commit(&concurrent).unwrap();
    release.notify_one();
    let result = mutation.await.unwrap();
    server.await.unwrap();
    assert!(
        matches!(
            result,
            Err(CoreSessionError::Config(
                ControlledConfigError::Transaction(_)
            ))
        ),
        "{result:?}"
    );
    if missing_cache {
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(&zenclash_i18n::text("backup.errors.no_runtime_snapshot"))
        );
        assert!(!store.runtime_path().exists());
        assert_eq!(
            payloads.lock().len(),
            1,
            "missing applied cache must not trigger a guessed runtime restore"
        );
        assert_eq!(state.lock().await.0, 8012);
        assert!(session.generation() > read_generation);
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    let restored: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
    let runtime = state.lock().await.clone();
    std::fs::remove_dir_all(root).unwrap();
    if changed_sources {
        assert_eq!(
            payloads.lock().last(),
            Some(&applied_payload),
            "rollback rebuilt modified source or override files instead of restoring the applied payload"
        );
    }
    assert_eq!(runtime, (8011, "rule".to_owned()));
    assert_eq!(restored["mixed-port"].as_u64(), Some(8011));
    assert_eq!(restored["mode"].as_str(), Some("rule"));
    if mode_update {
        assert_eq!(temporary.mode, "global");
    } else {
        assert_eq!(temporary.mixed_port, 8012);
    }
    assert!(
        session.generation() > read_generation,
        "temporary runtime query remained current after direct rollback"
    );
}

#[derive(Clone, Copy)]
enum QueuedConfigUpdate {
    Mode,
    Patch,
    Reapply,
}

async fn queued_update_uses_committed_config(update: QueuedConfigUpdate) {
    let _ports = fixed_listener_ports_guard().await;
    let complete_reload = !matches!(update, QueuedConfigUpdate::Mode);
    let root = std::env::temp_dir().join(format!(
        "zenclash-queued-mode-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let a = root.join("a.yaml");
    let b = root.join("b.yaml");
    let previous_override = root.join("previous-override.yaml");
    let next_override = root.join("next-override.yaml");
    std::fs::write(&a, "mixed-port: 8011\nrules: [MATCH,DIRECT]\n").unwrap();
    std::fs::write(&b, "mixed-port: 8012\nrules: [MATCH,DIRECT]\n").unwrap();
    std::fs::write(&previous_override, "allow-lan: false\n").unwrap();
    std::fs::write(&next_override, "allow-lan: true\n").unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let mut entered = Some(entered);
        let mut requests = Vec::new();
        for index in 0..if complete_reload { 2 } else { 4 } {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_controller_request_blocking(&mut stream).unwrap();
            requests.push(String::from_utf8(request.into_bytes()).unwrap());
            if index == 0 {
                entered.take().unwrap().send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            if complete_reload || matches!(index, 0 | 2) {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            } else {
                let mode = if index == 1 { "rule" } else { "global" };
                let body = format!(r#"{{"mode":"{mode}"}}"#);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = CoreSession::open_with_config(
        CoreKind::Mihomo,
        client,
        Some(a.clone()),
        vec![previous_override.clone()],
    )
    .unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let activation = {
        let session = session.clone();
        let store = store.clone();
        tokio::spawn(async move {
            session
                .apply(
                    &store,
                    EffectiveConfigIntent::ActivateProfile {
                        profile: b,
                        overrides: vec![next_override],
                    },
                )
                .await
        })
    };
    waiting.await.unwrap();
    let mode = {
        let session = session.clone();
        let store = store.clone();
        tokio::spawn(async move {
            if matches!(update, QueuedConfigUpdate::Reapply) {
                session
                    .apply(
                        &store,
                        EffectiveConfigIntent::ReapplyCurrent {
                            overrides: vec![previous_override],
                        },
                    )
                    .await
                    .map(|outcome| outcome.generation)
            } else if matches!(update, QueuedConfigUpdate::Patch) {
                session
                    .apply(
                        &store,
                        EffectiveConfigIntent::Patch {
                            profile: a,
                            patch: serde_json::json!({"mode": "global"}),
                            overrides: vec![previous_override],
                        },
                    )
                    .await
                    .map(|outcome| outcome.generation)
            } else {
                session.set_mode(&store, "global").await
            }
        })
    };
    release.send(()).unwrap();
    assert_eq!(activation.await.unwrap().unwrap().generation, 1);
    assert_eq!(mode.await.unwrap().unwrap(), 2);
    assert_eq!(
        session.committed_profile_snapshot(),
        CoreCommittedProfileSnapshot {
            profile_path: Some(root.join("b.yaml")),
            generation: 2,
        }
    );
    let runtime: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(store.runtime_path()).unwrap()).unwrap();
    assert_eq!(runtime["mixed-port"].as_u64(), Some(8012));
    if !matches!(update, QueuedConfigUpdate::Reapply) {
        assert_eq!(runtime["mode"].as_str(), Some("global"));
    }
    assert_eq!(
        runtime["allow-lan"].as_bool(),
        Some(!matches!(update, QueuedConfigUpdate::Reapply))
    );
    let requests = server.join().unwrap();
    assert!(
        requests[if complete_reload { 1 } else { 2 }].starts_with(if complete_reload {
            "PUT /configs?force=true "
        } else {
            "PATCH /configs "
        })
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_mode_does_not_run_after_shutdown_is_requested() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let guard = session.transition.lock().await;
    let task = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .set_mode(&ControlledConfigStore::new(std::env::temp_dir()), "global")
                .await
        })
    };
    session.request_shutdown();
    drop(guard);
    assert!(matches!(
        task.await.unwrap(),
        Err(CoreSessionError::ShuttingDown)
    ));
    assert_eq!(session.generation(), 0);
}

#[tokio::test]
async fn accepted_profile_apply_reports_hot_reload_and_advances_generation() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = String::from_utf8(
            read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes(),
        )
        .unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .unwrap();
        request
    });
    let root = std::env::temp_dir().join(format!(
        "zenclash-core-session-apply-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("profile.yaml");
    std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let client =
        MihomoClient::new(crate::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();

    let outcome = session
        .apply(
            &store,
            EffectiveConfigIntent::ActivateProfile {
                profile,
                overrides: Vec::new(),
            },
        )
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
    assert_eq!(outcome.generation, 1);
    assert_eq!(session.snapshot().generation, 1);
    assert!(request.starts_with("PUT /configs?force=true "));
    assert!(store.runtime_path().is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn uncertain_external_mihomo_apply_never_becomes_an_owned_restart() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_controller_request_blocking(&mut stream).unwrap();
        assert!(!request.headers().is_empty());
    });
    let root = std::env::temp_dir().join(format!(
        "zenclash-core-session-external-uncertain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("profile.yaml");
    std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        session.apply(
            &store,
            EffectiveConfigIntent::ActivateProfile {
                profile,
                overrides: Vec::new(),
            },
        ),
    )
    .await
    .expect("external controller failure did not remain bounded")
    .unwrap_err();
    server.join().unwrap();

    assert!(matches!(
        error,
        CoreSessionError::Config(ControlledConfigError::Profile(MihomoError::Http(_)))
    ));
    let snapshot = session.snapshot();
    assert!(!snapshot.managed);
    assert!(
        snapshot.running.is_none(),
        "external process ownership is not inferred"
    );
    assert_eq!(
        snapshot.generation, 1,
        "the uncertain reload must invalidate previous runtime reads"
    );
    assert_eq!(
        session.lifecycle_snapshot().phase,
        CoreLifecyclePhase::External
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_config_rejection_is_not_a_restart_signal() {
    let error = ControlledConfigError::Profile(MihomoError::Api {
        status: 400,
        message: "invalid config".into(),
    });

    assert!(!should_restart_after_hot_reload(&error));
}

#[test]
fn annotated_decode_errors_preserve_transport_restart_policy() {
    for outcome_unknown in [false, true] {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = ControlledConfigError::Profile(MihomoError::ResponseDecode {
            endpoint: "/configs".into(),
            field_path: "$".into(),
            source,
            outcome_unknown,
        });
        assert_eq!(should_restart_after_hot_reload(&error), outcome_unknown);
    }
}

use super::test_support::session_test_root;
