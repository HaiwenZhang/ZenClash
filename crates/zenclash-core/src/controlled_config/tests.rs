use std::{
    fs,
    io::{BufRead, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{ControlledConfigError, ControlledConfigStore, normalize_runtime_payload};
use crate::{CoreKind, MihomoClient, MihomoEndpoint};

fn test_root(name: &str) -> PathBuf {
    let sequence = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "zenclash-controlled-{name}-{}-{sequence}",
        std::process::id()
    ))
}

fn write_profile(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join("base.yaml");
    fs::write(
        &path,
        "mixed-port: 7890\ndns:\n  enable: true\n  nameserver: [1.1.1.1]\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    path
}

#[tokio::test]
async fn service_tun_preparation_normalizes_held_and_saved_yaml_without_polluting_user_patch() {
    let root = test_root("service-tun-defaults");
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("base.yaml");
    fs::write(
        &profile,
        "ipv6: true\ntun: {enable: false}\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    let before = store.cached_runtime_payload().unwrap().unwrap();
    let bundle = crate::ServiceRuntimeBundle::prepare(&before, root.clone())
        .await
        .unwrap();
    let held = bundle
        .with_delta(&serde_json::json!({"tun": {"enable": true}}))
        .unwrap();
    let update = store
        .prepare_service_tun_update(profile, vec![], None)
        .await
        .unwrap();
    let saved: serde_yaml::Value = serde_yaml::from_str(update.next_payload()).unwrap();
    let frozen: serde_yaml::Value = serde_yaml::from_str(held.yaml()).unwrap();
    assert_eq!(saved["tun"], frozen["tun"]);
    assert_eq!(saved["dns"], frozen["dns"]);
    assert_eq!(saved["dns"]["fake-ip-range6"], "2001:2::0/64");
    let patch: serde_yaml::Value = serde_yaml::from_slice(&update.next_patch).unwrap();
    assert_eq!(
        patch,
        serde_yaml::from_str::<serde_yaml::Value>("tun: {enable: true}\n").unwrap()
    );
    assert_eq!(
        store.cached_runtime_payload().unwrap().as_deref(),
        Some(before.as_str())
    );
    assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_tun_projection_overrides_yaml_layers_without_erasing_saved_intent() {
    let root = test_root("startup-tun-projection");
    let profile = write_profile(&root);
    fs::write(&profile, "tun: {enable: true}\nmode: rule\n").unwrap();
    let yaml_override = root.join("user.yaml");
    fs::write(&yaml_override, "tun: {enable: true, stack: mixed}\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let projected = store.without_startup_tun();
    let payload = projected
        .effective_with_overrides(&profile, std::slice::from_ref(&yaml_override))
        .unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_str(&payload).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(yaml["tun"]["stack"].as_str(), Some("mixed"));
    assert!(
        crate::tun_admission::yaml_enables_tun(
            &store
                .effective_with_overrides(&profile, &[yaml_override])
                .unwrap()
        )
        .unwrap()
    );
    assert!(
        fs::read_to_string(&profile)
            .unwrap()
            .contains("enable: true")
    );
    assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ordinary_runtime_updates_keep_tun_projection_and_service_preparation_retains_intent() {
    let root = test_root("runtime-tun-projection");
    let profile = write_profile(&root);
    fs::write(&profile, "tun: {enable: true}\nmode: rule\n").unwrap();
    let binary = root.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    fs::write(&binary, b"synthetic stopped owner; never executed").unwrap();
    let process = crate::MihomoProcess::prepare_stopped(crate::MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: profile.clone(),
        home_dir: root.join("home"),
        endpoint: MihomoEndpoint::default(),
        controller_override: None,
    });
    let client = MihomoClient::from_process(process).unwrap();
    let store = ControlledConfigStore::new(root.join("store")).with_runtime_tun_policy(&client);
    let ordinary = store
        .prepare_service_config_update(
            profile.clone(),
            Some(serde_json::json!({"mode":"direct"})),
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        crate::tun_admission::yaml_enables_tun(ordinary.next_payload()).unwrap(),
        crate::current_process_elevated()
    );
    let service = store
        .for_service_runtime()
        .prepare_service_config_update(
            profile,
            Some(serde_json::json!({"mode":"direct"})),
            vec![],
            None,
        )
        .await
        .unwrap();
    assert!(crate::tun_admission::yaml_enables_tun(service.next_payload()).unwrap());
    assert!(
        store
            .runtime_tun_policy
            .as_ref()
            .unwrap()
            .upgrade()
            .is_some()
    );
    assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
    drop(client);
    assert!(
        store
            .runtime_tun_policy
            .as_ref()
            .unwrap()
            .upgrade()
            .is_none(),
        "projection must not keep runtime owners alive"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn local_recovery_save_conflict_restores_cache_and_preserves_external_patch() {
    let root = test_root("local-recovery-save-conflict");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let before = fs::read(store.runtime_path()).unwrap();
    let (gate, entered, release) = super::CommitGate::new();
    store.commit_gate = Some(gate);
    let lease = store.acquire_write_lease().await.unwrap();
    let mutation = store.lock_service_tun_mutation().await;
    let borrowed = store.with_write_lease(&lease);
    let task = tokio::spawn(async move {
        let _lease = lease;
        let _mutation = mutation;
        borrowed
            .persist_local_recovery_payload_admitted("tun:\n  enable: false\n".into())
            .await
    });
    entered.await.unwrap();
    let replacement = b"mode: direct\n";
    fs::write(store.patch_path(), replacement).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(ControlledConfigError::ConcurrentModification)
    ));
    assert_eq!(fs::read(store.runtime_path()).unwrap(), before);
    assert_eq!(fs::read(store.patch_path()).unwrap(), replacement);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_local_recovery_save_waiter_does_not_abandon_admitted_persistence() {
    let root = test_root("local-recovery-save-cancel");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let (gate, entered, release) = super::CommitGate::new();
    store.commit_gate = Some(gate);
    let bundle = std::sync::Arc::new(
        crate::ServiceRuntimeBundle::prepare("tun:\n  enable: false\n", root.clone())
            .await
            .unwrap(),
    );
    let waiting_store = store.clone();
    let home = root.clone();
    let waiter = tokio::spawn(async move {
        bundle
            .with_local_geodata(&waiting_store, home, |recovery| async move {
                recovery
                    .persist_local_payload("tun:\n  enable: false\n".into())
                    .await
                    .map_err(|error| crate::MihomoError::Process(error.to_string()))
            })
            .await
    });
    entered.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let _finished = store.lock_service_tun_mutation().await;
    assert_eq!(
        store.load().unwrap()["tun"]["enable"].as_bool(),
        Some(false)
    );
    assert_eq!(
        serde_yaml::from_slice::<serde_yaml::Value>(&fs::read(store.runtime_path()).unwrap())
            .unwrap()["tun"]["enable"]
            .as_bool(),
        Some(false)
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn service_tun_recovery_preparation_uses_held_payload_without_sources_or_cache() {
    let root = test_root("service-tun-recovery-held");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let bundle = std::sync::Arc::new(
        crate::ServiceRuntimeBundle::prepare(
            "mode: rule\ntun:\n  enable: false\ndns:\n  enable: false\nrules: [MATCH,DIRECT]\n",
            root.clone(),
        )
        .await
        .unwrap(),
    );
    fs::remove_file(profile).unwrap();
    fs::write(store.runtime_path(), b"[broken cache").unwrap();
    let before_layer = store.load().unwrap();
    let update = store
        .prepare_service_tun_recovery_update(bundle.clone())
        .await
        .unwrap();
    let previous: serde_yaml::Value = serde_yaml::from_str(update.previous_payload()).unwrap();
    let next: serde_yaml::Value = serde_yaml::from_str(update.next_payload()).unwrap();
    assert_eq!(previous["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(next["tun"]["enable"].as_bool(), Some(true));
    assert_eq!(next["dns"]["enable"].as_bool(), Some(true));
    assert_eq!(next["mode"].as_str(), Some("rule"));
    assert_eq!(store.load().unwrap(), before_layer);
    assert_eq!(fs::read(store.runtime_path()).unwrap(), b"[broken cache");
    assert_eq!(
        serde_yaml::from_str::<serde_yaml::Value>(bundle.yaml()).unwrap()["tun"]["enable"]
            .as_bool(),
        Some(false)
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn profile_mode_replaced_after_preflight_sends_no_patch_and_keeps_generation() {
    profile_mode_replaced_after_preflight(false).await;
}

#[tokio::test]
async fn profile_mode_zero_send_reports_cache_failure_without_runtime_change() {
    profile_mode_replaced_after_preflight(true).await;
}

async fn profile_mode_replaced_after_preflight(fail_cache_restore: bool) {
    use std::{task::Context, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = test_root("profile-mode-zero-send");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let previous_cache = fs::read(store.runtime_path()).unwrap();
    let previous_layer = store.load().unwrap();
    let (gate, entered, release) = super::ModePatchGate::new();
    store.mode_patch_gate = Some(gate);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.unwrap());
        }
        assert!(headers.starts_with(b"GET /configs "));
        let body = r#"{"mode":"rule"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(300), listener.accept())
            .await
            .is_ok()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = crate::CoreSession::open_with_config(
        CoreKind::Mihomo,
        client.clone(),
        Some(profile),
        vec![],
    )
    .unwrap();
    let operation = {
        let session = session.clone();
        let store = store.clone();
        tokio::spawn(async move { session.set_mode(&store, "global").await })
    };
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    client
        .switch_to_direct(MihomoEndpoint::new(
            format!("http://{}", replacement.local_addr().unwrap()),
            "",
        ))
        .await
        .unwrap();
    if fail_cache_restore {
        fs::remove_file(store.runtime_path()).unwrap();
        fs::create_dir(store.runtime_path()).unwrap();
    }
    drop(release);
    let result = tokio::time::timeout(Duration::from_secs(5), operation)
        .await
        .unwrap()
        .unwrap();
    if fail_cache_restore {
        assert!(matches!(
            result,
            Err(crate::CoreSessionError::Config(ControlledConfigError::Io(
                _
            )))
        ));
        assert!(store.runtime_path().is_dir());
    } else {
        assert!(matches!(
            result,
            Err(crate::CoreSessionError::Config(
                ControlledConfigError::Profile(crate::MihomoError::StaleBinding)
            ))
        ));
        assert_eq!(fs::read(store.runtime_path()).unwrap(), previous_cache);
    }
    assert_eq!(session.generation(), 0);
    assert_eq!(store.load().unwrap(), previous_layer);
    assert!(
        replacement
            .poll_accept(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    assert!(
        !server.await.unwrap(),
        "zero-send rejection attempted a controller rollback"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reload_rejects_a_binding_switched_while_waiting_for_store_mutation() {
    use std::{future::Future, task::Context, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = test_root("reload-binding-admission");
    let profile = write_profile(&root);
    let lease = crate::data_coordinator::DataWriteLease::shared([root.clone()]);
    let store = ControlledConfigStore::new(root.join("store")).with_write_lease(&lease);
    store.materialize(&profile).unwrap();
    let previous_cache = fs::read(store.runtime_path()).unwrap();
    let client = MihomoClient::new(MihomoEndpoint::default())
        .unwrap()
        .with_config_validator(crate::CoreConfigValidator::new(
            CoreKind::Mihomo,
            root.join("unused-kernel"),
            root.join("home"),
        ))
        .unwrap();
    let mutation = store.mutation_gate.lock().await;
    let mut operation = Box::pin(store.reload_profile(&client, &profile));
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
        let mut request = [0_u8; 4096];
        assert_ne!(stream.read(&mut request).await.unwrap(), 0);
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
    drop(mutation);
    assert!(matches!(
        operation.await,
        Err(ControlledConfigError::Profile(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert!(
        !server.await.unwrap(),
        "stale reload reached the replacement controller"
    );
    assert_eq!(fs::read(store.runtime_path()).unwrap(), previous_cache);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_the_waiter_keeps_patch_cache_and_session_consistent_with_persistence() {
    let root = test_root("cancel-patch-persistence");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let (gate, waiting, release) = super::CommitGate::new();
    store.commit_gate = Some(gate);
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(&mut stream);
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        drop(reader);
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let session = crate::CoreSession::open_with_config(
        CoreKind::Mihomo,
        client,
        Some(profile.clone()),
        vec![],
    )
    .unwrap();
    let worker_session = session.clone();
    let worker_store = store.clone();
    let outer = tokio::spawn(async move {
        worker_session
            .apply(
                &worker_store,
                crate::EffectiveConfigIntent::Patch {
                    profile,
                    patch: serde_json::json!({"mode": "global"}),
                    overrides: vec![],
                },
            )
            .await
    });
    waiting.await.unwrap();
    outer.abort();
    assert!(outer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if store
                .load_json()
                .unwrap()
                .get("mode")
                .and_then(serde_json::Value::as_str)
                == Some("global")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    session.shutdown().await.unwrap();
    server.join().unwrap();
    assert!(
        fs::read_to_string(store.runtime_path())
            .unwrap()
            .contains("mode: global")
    );
    // Both the persisted application and confirmed shutdown advance the session.
    assert_eq!(session.generation(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_monitor_ignores_applied_writes_and_detects_external_effective_changes() {
    let root = test_root("source-monitor");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    assert_eq!(
        store
            .pending_source_revision(CoreKind::Mihomo, &profile, &[])
            .unwrap(),
        None
    );
    let payload = fs::read_to_string(&profile).unwrap();
    fs::write(&profile, format!("# external editor save\n{payload}")).unwrap();
    assert_eq!(
        store
            .pending_source_revision(CoreKind::Mihomo, &profile, &[])
            .unwrap(),
        None
    );
    fs::write(&profile, payload.replace("7890", "7891")).unwrap();
    assert!(
        store
            .pending_source_revision(CoreKind::Mihomo, &profile, &[])
            .unwrap()
            .is_some()
    );
    store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    assert_eq!(
        store
            .pending_source_revision(CoreKind::Mihomo, &profile, &[])
            .unwrap(),
        None
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_monitor_rejects_invalid_yaml_without_touching_runtime_cache() {
    let root = test_root("invalid-source-monitor");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("controlled"));
    let runtime = store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    let original = fs::read(&runtime).unwrap();
    fs::write(&profile, "rules: [").unwrap();
    assert!(
        store
            .pending_source_revision(CoreKind::Mihomo, &profile, &[])
            .is_err()
    );
    assert_eq!(fs::read(runtime).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_controlled_patch_is_quarantined_without_deleting_its_payload() {
    let root = test_root("quarantine-invalid");
    let store = ControlledConfigStore::new(root.join("store"));
    fs::create_dir_all(store.root()).unwrap();
    let invalid = "- this\n- is\n- not-a-mapping\n";
    fs::write(store.root().join("override.yaml"), invalid).unwrap();

    assert!(matches!(
        store.load(),
        Err(ControlledConfigError::NotMapping)
    ));
    let quarantine = store.quarantine_invalid_patch().unwrap().unwrap();

    assert_eq!(fs::read_to_string(quarantine).unwrap(), invalid);
    assert!(store.load().unwrap().as_mapping().unwrap().is_empty());
    assert!(store.quarantine_invalid_patch().unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_listener_fallback_is_session_only_and_survives_cache_regeneration() {
    let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let original_port = occupied.local_addr().unwrap().port();
    let root = test_root("listener-fallback");
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("base.yaml");
    let source = format!("mixed-port: {original_port}\nallow-lan: false\nrules: [MATCH,DIRECT]\n");
    fs::write(&profile, &source).unwrap();
    let store = ControlledConfigStore::new(root.join("store"));

    store.materialize(&profile).unwrap();
    let fallbacks = store.resolve_startup_listener_conflicts().unwrap();

    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].listener, "mixed-port");
    assert_eq!(fallbacks[0].original, original_port);
    assert_ne!(fallbacks[0].current, original_port);
    assert_eq!(fs::read_to_string(&profile).unwrap(), source);

    store.materialize(&profile).unwrap();
    let regenerated = fs::read_to_string(store.runtime_path()).unwrap();
    assert!(regenerated.contains(&format!("mixed-port: {}", fallbacks[0].current)));

    let explicit_port = if original_port == 23_456 {
        23_457
    } else {
        23_456
    };
    fs::write(
        &profile,
        format!("mixed-port: {explicit_port}\nallow-lan: false\nrules: [MATCH,DIRECT]\n"),
    )
    .unwrap();
    store.materialize(&profile).unwrap();
    assert!(
        fs::read_to_string(store.runtime_path())
            .unwrap()
            .contains(&format!("mixed-port: {explicit_port}"))
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn settings_update_rejects_an_occupied_listener_before_persisting() {
    let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let root = test_root("occupied-settings-port");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    let client = MihomoClient::new(MihomoEndpoint::new("http://127.0.0.1:0", "")).unwrap();

    let result = store
        .apply_json_update(
            &client,
            &profile,
            &serde_json::json!({"mixed-port": occupied_port}),
        )
        .await;

    assert!(matches!(
        result,
        Err(ControlledConfigError::ListenerFallback(_))
    ));
    assert!(store.load_json().unwrap().get("mixed-port").is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepares_commits_and_materializes_without_changing_source_profile() {
    let root = test_root("commit");
    let profile = write_profile(&root);
    let original = fs::read(&profile).unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let update = store
        .prepare_json_update(
            &profile,
            &serde_json::json!({"dns": {"enable": false, "ipv6": true}}),
        )
        .unwrap();

    assert!(update.next_payload().contains("enable: false"));
    store.commit(&update).unwrap();
    let effective = store.materialize(&profile).unwrap();

    assert_eq!(fs::read(&profile).unwrap(), original);
    assert_eq!(
        fs::read_to_string(effective).unwrap(),
        update.next_payload()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn meow_materialization_adds_dns_upstreams_only_for_an_empty_enabled_resolver() {
    let root = test_root("meow-dns-defaults");
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("base.yaml");
    fs::write(
        &profile,
        "mixed-port: 7890\ndns:\n  enable: true\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = ControlledConfigStore::new(root.join("store"));

    let effective = store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Meow)
        .unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(effective).unwrap()).unwrap();

    assert_eq!(yaml["dns"]["nameserver"].as_sequence().unwrap().len(), 2);
    assert_eq!(
        yaml["dns"]["default-nameserver"]
            .as_sequence()
            .unwrap()
            .len(),
        2
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mihomo_materialization_adds_geodata_fallbacks_without_rewriting_the_source() {
    let root = test_root("mihomo-geodata-defaults");
    let profile = write_profile(&root);
    let original = fs::read(&profile).unwrap();
    let store = ControlledConfigStore::new(root.join("store"));

    let effective = store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(effective).unwrap()).unwrap();

    assert_eq!(
        yaml["geox-url"]["mmdb"].as_str(),
        Some("https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.metadb")
    );
    assert!(yaml["geox-url"]["geoip"].as_str().is_some());
    assert!(yaml["geox-url"]["geosite"].as_str().is_some());
    assert_eq!(fs::read(profile).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mihomo_materialization_preserves_explicit_geodata_urls() {
    let payload = "geox-url:\n  mmdb: https://example.com/custom.mmdb\nrules: [MATCH,DIRECT]\n";

    let normalized = normalize_runtime_payload(CoreKind::Mihomo, payload.into()).unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_str(&normalized).unwrap();

    assert_eq!(
        yaml["geox-url"]["mmdb"].as_str(),
        Some("https://example.com/custom.mmdb")
    );
    assert!(yaml["geox-url"]["geoip"].as_str().is_some());
    assert!(yaml["geox-url"]["geosite"].as_str().is_some());
}

#[test]
fn core_materialization_preserves_explicit_dns_upstreams() {
    let root = test_root("meow-custom-dns");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));

    let effective = store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Meow)
        .unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(effective).unwrap()).unwrap();

    assert_eq!(yaml["dns"]["nameserver"][0].as_str(), Some("1.1.1.1"));
    assert!(yaml["dns"]["default-nameserver"].is_null());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn meow_materialization_keeps_custom_bootstrap_and_adds_missing_main_dns() {
    let root = test_root("meow-bootstrap-only");
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("base.yaml");
    fs::write(
        &profile,
        "dns:\n  enable: true\n  default-nameserver: [9.9.9.9]\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = ControlledConfigStore::new(root.join("store"));

    let effective = store
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Meow)
        .unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(effective).unwrap()).unwrap();

    assert_eq!(yaml["dns"]["nameserver"].as_sequence().unwrap().len(), 2);
    assert_eq!(
        yaml["dns"]["default-nameserver"][0].as_str(),
        Some("9.9.9.9")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_prepared_update_cannot_overwrite_a_newer_commit() {
    let root = test_root("stale");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    let first = store
        .prepare_json_update(&profile, &serde_json::json!({"ipv6": true}))
        .unwrap();
    let stale = store
        .prepare_json_update(&profile, &serde_json::json!({"ipv6": false}))
        .unwrap();
    store.commit(&first).unwrap();

    assert!(matches!(
        store.commit(&stale),
        Err(ControlledConfigError::ConcurrentModification)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn effective_json_exposes_merged_values_for_native_forms() {
    let root = test_root("effective-json");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    let update = store
        .prepare_json_update(
            &profile,
            &serde_json::json!({"dns": {"nameserver": ["https://dns.example/dns-query"]}}),
        )
        .unwrap();
    store.commit(&update).unwrap();

    let effective = store.effective_json(&profile).unwrap();
    assert_eq!(
        effective
            .pointer("/dns/nameserver/0")
            .and_then(serde_json::Value::as_str),
        Some("https://dns.example/dns-query")
    );
    assert_eq!(
        effective
            .pointer("/mixed-port")
            .and_then(serde_json::Value::as_u64),
        Some(7890)
    );
    let source = store.source_payload(&profile).unwrap();
    let effective_yaml = store.effective_payload(&profile).unwrap();
    let diff = crate::diff_yaml_configs(&source, &effective_yaml, 20).unwrap();
    assert!(
        diff.entries
            .iter()
            .any(|entry| entry.path == "/dns/nameserver")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dropping_staged_runtime_cache_restores_previous_startup_payload() {
    let root = test_root("runtime-cache-drop");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    let runtime_path = store.materialize(&profile).unwrap();
    let previous = fs::read(&runtime_path).unwrap();

    drop(store.stage_runtime_payload("mixed-port: 9999\n").unwrap());

    assert_eq!(fs::read(runtime_path).unwrap(), previous);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reload_profile_updates_managed_startup_cache_after_mihomo_accepts_payload() {
    let root = test_root("runtime-cache-accept");
    let first = write_profile(&root);
    let second = root.join("second.yaml");
    fs::write(
        &second,
        "mixed-port: 0\ndns:\n  enable: true\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let runtime_path = store.materialize(&first).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let length = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8_lossy(&request[..length]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    store.reload_profile(&client, &second).await.unwrap();

    assert!(
        fs::read_to_string(runtime_path)
            .unwrap()
            .contains("mixed-port: 0")
    );
    assert!(server.join().unwrap().contains("mixed-port: 0"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reload_profile_restores_startup_cache_when_mihomo_rejects_payload() {
    let root = test_root("runtime-cache-reject");
    let first = write_profile(&root);
    let second = root.join("second.yaml");
    fs::write(&second, "mixed-port: 0\nrules: [MATCH,DIRECT]\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let runtime_path = store.materialize(&first).unwrap();
    let previous = fs::read(&runtime_path).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"message":"rejected"}"#;
        write!(
            stream,
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let result = store.reload_profile(&client, second).await;

    assert!(matches!(result, Err(ControlledConfigError::Profile(_))));
    assert_eq!(fs::read(runtime_path).unwrap(), previous);
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn profile_rollback_restores_the_exact_applied_payload_after_sources_change() {
    let root = test_root("profile-rollback-applied-snapshot");
    fs::create_dir_all(&root).unwrap();
    let previous_profile = root.join("previous.yaml");
    let candidate = root.join("candidate.yaml");
    let previous_override = root.join("previous-override.yaml");
    let next_override = root.join("next-override.yaml");
    fs::write(&previous_profile, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
    fs::write(&candidate, "mode: rule\nrules: ['MATCH,REJECT']\n").unwrap();
    fs::write(&previous_override, "mode: direct\n").unwrap();
    fs::write(&next_override, "mode: global\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    store
        .materialize_with_overrides_for_core(
            &previous_profile,
            &[previous_override],
            CoreKind::Mihomo,
        )
        .unwrap();
    let applied_payload = fs::read_to_string(store.runtime_path()).unwrap();
    fs::write(&previous_profile, "mode: global\nrules: ['MATCH,REJECT']\n").unwrap();
    let (client, requests) = reload_fixture(2);

    let transaction = store
        .stage_profile_reload(
            &client,
            candidate,
            Some(previous_profile),
            vec![next_override],
        )
        .await
        .unwrap();
    transaction.rollback().await.unwrap();

    let requests = requests.join().unwrap();
    assert_eq!(
        requests[1]["payload"].as_str(),
        Some(applied_payload.as_str())
    );
    assert_eq!(
        fs::read_to_string(store.runtime_path()).unwrap(),
        applied_payload
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn accepted_profile_without_an_applied_snapshot_reports_unknown_on_rollback() {
    let root = test_root("profile-rollback-without-snapshot");
    let profile = write_profile(&root);
    let candidate = root.join("candidate.yaml");
    fs::write(&candidate, "mode: global\nrules: ['MATCH,REJECT']\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let (client, requests) = reload_fixture(1);

    let transaction = store
        .stage_profile_reload(&client, candidate, Some(profile), Vec::new())
        .await
        .unwrap();
    assert!(store.runtime_path().exists());
    let error = transaction.rollback().await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains(&zenclash_i18n::text("backup.errors.no_runtime_snapshot"))
    );
    assert!(!store.runtime_path().exists());
    assert_eq!(requests.join().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn frozen_local_patch_save_failure_restores_exact_runtime_and_cache() {
    let root = test_root("frozen-local-save-failure");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let previous = store.cached_runtime_payload().unwrap().unwrap();
    let update = store
        .prepare_service_config_update(
            profile.clone(),
            Some(serde_json::json!({"mode":"global"})),
            vec![],
            Some(previous.clone()),
        )
        .await
        .unwrap();
    let next = update.next_payload().to_owned();
    let (gate, entered, release) = super::CommitGate::new();
    store.commit_gate = Some(gate);
    let (client, requests) = reload_fixture(2);
    let worker = store.clone();
    let expected = previous.clone();
    let task = tokio::spawn(async move {
        worker
            .apply_frozen_local_update(
                &client,
                update,
                Some(expected),
                true,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered)
        .await
        .unwrap()
        .unwrap();
    fs::write(&profile, "tun: {enable: true}\nmode: direct\n").unwrap();
    fs::create_dir(store.patch_path()).unwrap();
    release.send(()).unwrap();
    let error = task.await.unwrap().err().unwrap();
    assert!(error.attempted);
    assert_eq!(
        store.cached_runtime_payload().unwrap().as_deref(),
        Some(previous.as_str())
    );
    let requests = requests.join().unwrap();
    assert_eq!(requests[0]["payload"].as_str(), Some(next.as_str()));
    assert_eq!(requests[1]["payload"].as_str(), Some(previous.as_str()));
    fs::remove_dir_all(root).unwrap();
}

fn reload_fixture(count: usize) -> (MihomoClient, thread::JoinHandle<Vec<serde_json::Value>>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = thread::spawn(move || {
        (0..count)
            .map(|_| {
                let (stream, _) = listener.accept().unwrap();
                let mut stream = std::io::BufReader::new(stream);
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    assert!(std::io::BufRead::read_line(&mut stream, &mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                stream.read_exact(&mut body).unwrap();
                write!(
                    stream.get_mut(),
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                serde_json::from_slice(&body).unwrap()
            })
            .collect()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    (client, requests)
}

#[tokio::test]
async fn settings_update_preserves_ordered_overrides_in_runtime_and_cache() {
    let root = test_root("settings-with-overrides");
    let profile = write_profile(&root);
    let override_path = root.join("override.yaml");
    fs::write(&override_path, "dns:\n  ipv6: false\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let length = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8_lossy(&request[..length]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    store
        .apply_json_update_with_overrides(
            &client,
            &profile,
            &serde_json::json!({"dns": {"ipv6": true}}),
            vec![override_path],
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .load_json()
            .unwrap()
            .pointer("/dns/ipv6")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let runtime = fs::read_to_string(store.runtime_path()).unwrap();
    assert!(runtime.contains("ipv6: false"));
    assert!(server.join().unwrap().contains("ipv6: false"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn conflicting_yaml_mode_is_rejected_before_runtime_or_storage_changes() {
    let root = test_root("mode-override-conflict");
    let profile = write_profile(&root);
    let yaml_override = root.join("mode.yaml");
    fs::write(&yaml_override, "mode: rule\n").unwrap();
    let store = ControlledConfigStore::new(root.join("store"));
    store
        .materialize_with_overrides_for_core(
            &profile,
            std::slice::from_ref(&yaml_override),
            CoreKind::Mihomo,
        )
        .unwrap();
    let previous_runtime = fs::read(store.runtime_path()).unwrap();
    let previous_patch = store.load_json().unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop, stopped) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        while stopped.try_recv().is_err() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = [0_u8; 8192];
                    let read = stream.read(&mut bytes).unwrap();
                    let request = String::from_utf8_lossy(&bytes[..read]).into_owned();
                    let body = if request.starts_with("GET ") {
                        if requests.is_empty() {
                            r#"{"mode":"rule"}"#
                        } else {
                            r#"{"mode":"global"}"#
                        }
                    } else {
                        ""
                    };
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                    requests.push(request);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) => panic!("controller accept failed: {error}"),
            }
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let result = store
        .apply_mode_update_with_overrides(&client, &profile, "global", vec![yaml_override])
        .await;
    stop.send(()).unwrap();
    let requests = server.join().unwrap();

    assert!(
        matches!(result, Err(ControlledConfigError::ModeOverrideConflict { requested, effective })
            if requested == "global" && effective == "rule"),
        "a conflicting YAML mode was not reported as a typed conflict"
    );
    assert!(
        requests.is_empty(),
        "a rejected mode reached the controller"
    );
    assert_eq!(store.load_json().unwrap(), previous_patch);
    assert_eq!(fs::read(store.runtime_path()).unwrap(), previous_runtime);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn mode_update_uses_partial_runtime_patch_and_persists_the_selection() {
    let root = test_root("mode-partial-patch");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for (index, mode) in ["rule", "", "global"].into_iter().enumerate() {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let length = stream.read(&mut request).unwrap();
            requests.push(String::from_utf8_lossy(&request[..length]).into_owned());
            if index == 1 {
                write!(
                    stream,
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            } else {
                let body = format!(r#"{{"mode":"{mode}"}}"#);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    store
        .apply_mode_update_with_overrides(&client, &profile, "global", Vec::new())
        .await
        .unwrap();

    let requests = server.join().unwrap();
    let first_lines = requests
        .iter()
        .map(|request| request.lines().next().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        first_lines,
        [
            "GET /configs HTTP/1.1",
            "PATCH /configs HTTP/1.1",
            "GET /configs HTTP/1.1"
        ]
    );
    assert!(requests[1].contains(r#""mode":"global""#));
    assert_eq!(
        store
            .load_json()
            .unwrap()
            .get("mode")
            .and_then(serde_json::Value::as_str),
        Some("global")
    );
    assert!(
        fs::read_to_string(store.runtime_path())
            .unwrap()
            .contains("mode: global")
    );
    fs::remove_dir_all(root).unwrap();
}

fn mode_fixture() -> (MihomoClient, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let requests = thread::spawn(move || {
        let mut requests = Vec::new();
        for mode in [Some("rule"), None, Some("global")] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "mode request missing");
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("controller accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            drop(reader);
            if let Some(mode) = mode {
                assert!(first.starts_with("GET /configs "));
                let body = format!(r#"{{"mode":"{mode}"}}"#);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            } else {
                assert!(first.starts_with("PATCH /configs "));
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                    serde_json::json!({"mode":"global"})
                );
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            requests.push(first.trim().to_owned());
        }
        requests
    });
    (
        MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap(),
        requests,
    )
}

#[tokio::test]
async fn partial_mode_retains_accepted_payload_when_profile_source_changes() {
    let root = test_root("partial-mode-accepted-payload");
    let profile = write_profile(&root);
    let store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let accepted: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(store.runtime_path()).unwrap()).unwrap();
    fs::write(&profile, "mixed-port: 3456\nrules: [MATCH,REJECT]\n").unwrap();
    let (client, requests) = mode_fixture();
    store
        .apply_mode_update_with_overrides(&client, &profile, "global", vec![])
        .await
        .unwrap();
    assert_eq!(
        requests.join().unwrap(),
        [
            "GET /configs HTTP/1.1",
            "PATCH /configs HTTP/1.1",
            "GET /configs HTTP/1.1"
        ]
    );
    let mut expected = accepted;
    expected["mode"] = serde_yaml::Value::from("global");
    let cached: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(store.runtime_path()).unwrap()).unwrap();
    assert_eq!(
        cached, expected,
        "a partial mode update must not accept changed rules or ports"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_partial_mode_waiter_keeps_durable_cache_in_completion_owner() {
    let root = test_root("partial-mode-cancel-save");
    let profile = write_profile(&root);
    let mut store = ControlledConfigStore::new(root.join("store"));
    store.materialize(&profile).unwrap();
    let (gate, entered, release) = super::CommitGate::new();
    store.commit_gate = Some(gate);
    let (client, requests) = mode_fixture();
    let worker = store.clone();
    let outer = tokio::spawn(async move {
        worker
            .apply_mode_update_with_overrides(&client, profile, "global", vec![])
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered)
        .await
        .unwrap()
        .unwrap();
    outer.abort();
    assert!(outer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let persisted = store.load_json().unwrap();
            if persisted["mode"] == "global" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(requests.join().unwrap().len(), 3);
    let cache: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(store.runtime_path()).unwrap()).unwrap();
    assert_eq!(
        cache["mode"].as_str(),
        Some("global"),
        "saved mode lost its accepted startup payload when only the waiter was cancelled"
    );
    fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn full_candidate_keeps_the_supplied_previous_snapshot_when_cache_changes() {
    let root =
        std::env::temp_dir().join(format!("zenclash-held-config-cache-{}", std::process::id()));
    let store = ControlledConfigStore::new(&root);
    std::fs::create_dir_all(store.root()).unwrap();
    let previous = "tun: {enable: false}\nmode: direct\n";
    let changed = "tun: {enable: false}\nmode: global\n";
    std::fs::write(store.runtime_path(), changed).unwrap();
    let update = store
        .prepare_service_config_update(
            crate::profile::ProfileRuntimeSource::Frozen("tun: {enable: true}\n".into()),
            None,
            vec![],
            Some(previous.into()),
        )
        .await
        .unwrap();
    assert_eq!(update.previous_payload(), previous);
    assert_eq!(
        std::fs::read_to_string(store.runtime_path()).unwrap(),
        changed
    );
    assert!(matches!(
        store
            .validate_prepared_service_tun(update, Some(previous.into()))
            .await,
        Err(ControlledConfigError::ConcurrentModification)
    ));
    std::fs::remove_dir_all(root).unwrap();
}
