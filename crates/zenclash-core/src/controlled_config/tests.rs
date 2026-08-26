use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{ControlledConfigError, ControlledConfigStore};
use crate::{MihomoClient, MihomoEndpoint};

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
    assert!(diff
        .entries
        .iter()
        .any(|entry| entry.path == "/dns/nameserver"));
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
        "mixed-port: 9888\ndns:\n  enable: true\nrules: [MATCH,DIRECT]\n",
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

    assert!(fs::read_to_string(runtime_path)
        .unwrap()
        .contains("mixed-port: 9888"));
    assert!(server.join().unwrap().contains("mixed-port: 9888"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reload_profile_restores_startup_cache_when_mihomo_rejects_payload() {
    let root = test_root("runtime-cache-reject");
    let first = write_profile(&root);
    let second = root.join("second.yaml");
    fs::write(&second, "mixed-port: 9888\nrules: [MATCH,DIRECT]\n").unwrap();
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
    assert!(fs::read_to_string(store.runtime_path())
        .unwrap()
        .contains("mode: global"));
    fs::remove_dir_all(root).unwrap();
}
