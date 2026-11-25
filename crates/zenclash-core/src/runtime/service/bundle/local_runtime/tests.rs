use super::*;

fn home() -> super::super::test_support::TestHome {
    super::super::test_support::TestHome::new()
}

#[tokio::test]
async fn local_slots_reject_uncovered_lease_before_creating_recovery_files() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let unrelated = crate::ControlledConfigStore::new(home.0.join("unrelated"));
    let lease = unrelated.acquire_write_lease().await.unwrap();
    let mutation = store.lock_service_tun_mutation().await;
    let bundle = Arc::new(ServiceRuntimeBundle {
        yaml: "tun:\n  enable: true\n".into(),
        assets: vec![],
    });
    let result = bundle
        .materialize_local_runtime_admitted(&store, None, lease, mutation)
        .await;
    assert!(
        result.is_err(),
        "An unrelated lease must not authorize recovery"
    );
    assert!(!store.root().join("local-runtime").exists());
}

#[tokio::test]
async fn local_cache_export_keeps_new_caches_and_held_tls_without_reading_sources() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let bundle = Arc::new(ServiceRuntimeBundle {
        yaml: "proxy-providers:\n  nodes:\n    type: http\n    path: assets/providers/nodes.yaml\nrule-providers:\n  rules:\n    type: http\n    path: assets/providers/rules.mrs\n".into(),
        assets: vec![RuntimeAsset { path: "assets/tls/cert.pem".into(), bytes: Arc::from(&b"held certificate"[..]) }],
    });
    let config = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let slot = config.parent().unwrap();
    fs::create_dir_all(slot.join("assets/providers")).unwrap();
    let mut provider: Value =
        serde_yaml::from_str("proxies:\n- name: downloaded\n  type: http\n").unwrap();
    provider["proxies"][0]["certificate"] =
        Value::from(slot.join("assets/tls/cert.pem").to_str().unwrap());
    fs::write(
        slot.join("assets/providers/nodes.yaml"),
        serde_yaml::to_string(&provider).unwrap(),
    )
    .unwrap();
    fs::write(slot.join("assets/providers/rules.mrs"), b"new rules").unwrap();
    fs::remove_file(slot.join("assets/tls/cert.pem")).unwrap();
    let exported = bundle.export_local_caches(&store, config).await.unwrap();
    assert_eq!(exported.yaml(), bundle.yaml());
    assert_eq!(
        &*exported
            .assets
            .iter()
            .find(|asset| asset.path == "assets/tls/cert.pem")
            .unwrap()
            .bytes,
        b"held certificate"
    );
    assert_eq!(
        &*exported
            .assets
            .iter()
            .find(|asset| asset.path == "assets/providers/rules.mrs")
            .unwrap()
            .bytes,
        b"new rules"
    );
    let nodes: Value = serde_yaml::from_slice(
        &exported
            .assets
            .iter()
            .find(|asset| asset.path == "assets/providers/nodes.yaml")
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(
        nodes["proxies"][0]["certificate"].as_str(),
        Some("assets/tls/cert.pem")
    );
    assert_eq!(bundle.assets.len(), 1);
}

#[tokio::test]
async fn local_cache_export_missing_removes_old_cache_and_invalid_cache_preserves_bundle() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let bundle = Arc::new(ServiceRuntimeBundle {
        yaml: "rule-providers:\n  rules:\n    type: http\n    path: assets/providers/rules.mrs\n"
            .into(),
        assets: vec![RuntimeAsset {
            path: "assets/providers/rules.mrs".into(),
            bytes: Arc::from(&b"old"[..]),
        }],
    });
    let config = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let cache = config.parent().unwrap().join("assets/providers/rules.mrs");
    fs::remove_file(&cache).unwrap();
    assert!(
        bundle
            .export_local_caches(&store, config.clone())
            .await
            .unwrap()
            .assets
            .is_empty()
    );
    fs::create_dir(&cache).unwrap();
    assert!(bundle.export_local_caches(&store, config).await.is_err());
    assert_eq!(&*bundle.assets[0].bytes, b"old");
    assert!(
        bundle
            .export_local_caches(&store, home.0.join("outside/runtime.yaml"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn local_slots_rotate_after_mihomo_populates_previously_missing_caches() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let mut yaml = String::from("rule-providers:\n");
    for index in 0..MAX_ASSETS {
        yaml.push_str(&format!(
            "  cache-{index}:\n    type: http\n    path: assets/providers/cache-{index}.mrs\n"
        ));
    }
    let bundle = Arc::new(ServiceRuntimeBundle {
        yaml,
        assets: (0..4)
            .map(|index| RuntimeAsset {
                path: format!("assets/tls/{index}.pem"),
                bytes: Arc::from([1u8]),
            })
            .collect(),
    });
    let first = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let caches = first.parent().unwrap().join("assets/providers");
    fs::create_dir(&caches).unwrap();
    for index in 0..MAX_ASSETS {
        fs::write(caches.join(format!("cache-{index}.mrs")), [1]).unwrap();
    }
    let second = bundle
        .materialize_local_runtime(&store, Some(first.clone()))
        .await
        .unwrap();
    let third = bundle
        .materialize_local_runtime(&store, Some(second))
        .await
        .expect("a populated HTTP cache slot must remain eligible for rotation");
    assert_eq!(third, first);
    assert!(!caches.exists());
}

#[cfg(windows)]
#[tokio::test]
async fn local_slots_reject_junctioned_destination_without_writing_outside() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    fs::create_dir_all(store.root()).unwrap();
    let outside = home.0.join("outside");
    fs::create_dir(&outside).unwrap();
    let junction = store.root().join("local-runtime");
    let output = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(output.status.success());
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
            .await
            .unwrap(),
    );
    assert!(
        bundle
            .materialize_local_runtime(&store, None)
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::remove_dir(junction).unwrap();
}

#[tokio::test]
async fn local_slots_use_held_tls_and_provider_bytes_after_sources_are_deleted() {
    let home = home();
    fs::write(home.0.join("key.pem"), b"held private key").unwrap();
    fs::write(
        home.0.join("provider.yaml"),
        "proxies:\n- name: tls-node\n  type: trojan\n  private-key: key.pem\n",
    )
    .unwrap();
    let source = "tun:\n  enable: true\nproxy-providers:\n  file:\n    type: file\n    path: provider.yaml\n  missing:\n    type: http\n    url: https://example.com/cache\nrule-providers:\n  rules:\n    type: file\n    path: rules.mrs\n";
    fs::write(home.0.join("rules.mrs"), [0, 255, 1, 2]).unwrap();
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare(source, home.0.clone())
            .await
            .unwrap(),
    );
    fs::remove_file(home.0.join("key.pem")).unwrap();
    fs::remove_file(home.0.join("provider.yaml")).unwrap();
    fs::remove_file(home.0.join("rules.mrs")).unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let path = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let value: Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["tun"]["enable"].as_bool(), Some(false));
    let file = PathBuf::from(value["proxy-providers"]["file"]["path"].as_str().unwrap());
    let proxy: Value = serde_yaml::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(
        fs::read(proxy["proxies"][0]["private-key"].as_str().unwrap()).unwrap(),
        b"held private key"
    );
    assert_eq!(
        fs::read(value["rule-providers"]["rules"]["path"].as_str().unwrap()).unwrap(),
        [0, 255, 1, 2]
    );
    let missing = Path::new(
        value["proxy-providers"]["missing"]["path"]
            .as_str()
            .unwrap(),
    );
    assert!(missing.is_absolute() && missing.starts_with(path.parent().unwrap()));
    assert!(!missing.exists());
    assert!(bundle.yaml().contains("enable: true"));
}

#[tokio::test]
async fn local_slot_failure_preserves_active_configuration_and_resources() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
            .await
            .unwrap(),
    );
    let first = bundle
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let first_bytes = fs::read(&first).unwrap();
    let invalid_bundle = Arc::new(ServiceRuntimeBundle {
        yaml: "tun: broken\n".into(),
        assets: vec![],
    });
    assert!(
        invalid_bundle
            .materialize_local_runtime(&store, Some(first.clone()))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert!(!store.root().join("local-runtime/.slot1.pending").exists());
    let second = bundle
        .materialize_local_runtime(&store, Some(first.clone()))
        .await
        .unwrap();
    assert!(second.ends_with("slot1/runtime.yaml"));
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    let third = bundle
        .materialize_local_runtime(&store, Some(second.clone()))
        .await
        .unwrap();
    assert_eq!(third, first);
    assert!(second.exists());
}

#[tokio::test]
async fn local_slots_reject_an_unrelated_active_path_before_creating_directories() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
            .await
            .unwrap(),
    );
    assert!(
        bundle
            .materialize_local_runtime(&store, Some(home.0.join("unrelated.yaml")))
            .await
            .is_err()
    );
    assert!(!store.root().join("local-runtime").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn local_slots_reject_symlinked_destination_without_writing_outside() {
    let home = home();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    fs::create_dir_all(store.root()).unwrap();
    let outside = home.0.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, store.root().join("local-runtime")).unwrap();
    let bundle = Arc::new(
        ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
            .await
            .unwrap(),
    );
    assert!(
        bundle
            .materialize_local_runtime(&store, None)
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}
