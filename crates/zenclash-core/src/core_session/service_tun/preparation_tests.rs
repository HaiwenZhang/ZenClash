use super::*;

#[tokio::test]
async fn backup_local_recovery_retains_provider_and_tls_after_sources_are_deleted() {
    let fixture = crate::core_session::test_support::OwnedCoreFixture::with_config_path(
        "geodata-backup-held-recovery",
        "home/controlled/local-runtime/slot0/runtime.yaml",
    )
    .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    std::fs::create_dir_all(store.root()).unwrap();
    let old =
        "tun: {enable: false}\nproxy-providers:\n  held:\n    type: file\n    path: nodes.yaml\n";
    std::fs::write(store.runtime_path(), old).unwrap();
    std::fs::write(
        home.join("nodes.yaml"),
        "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
    )
    .unwrap();
    std::fs::write(home.join("cert.pem"), b"backup held certificate").unwrap();
    let candidate = crate::backup::BackupRuntimeCandidate {
        data_root: home.clone(),
        profile: home.join("next.yaml"),
        overrides: vec![],
        payload: "tun: {enable: false}\nmode: direct\n".into(),
        patch: b"{}\n".to_vec(),
    };
    let (_, snapshot) = session
        .prepare_service_backup_config(
            &store,
            crate::core_session::RuntimeConfigurationRevision {
                binding_generation: session.runtime_descriptor().binding_generation(),
                configuration_generation: 0,
            },
            &candidate,
        )
        .await
        .unwrap();
    std::fs::remove_file(home.join("nodes.yaml")).unwrap();
    std::fs::remove_file(home.join("cert.pem")).unwrap();
    std::fs::write(store.runtime_path(), "mode: direct\n").unwrap();
    let pid = fixture.process.snapshot().pid;
    let admission = session.begin_backup_restore().await.unwrap();
    session
        .restore_backup_snapshot(&store, &snapshot, &admission)
        .await
        .unwrap();
    let recovered: serde_yaml::Value =
        serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
    let provider = PathBuf::from(
        recovered["proxy-providers"]["held"]["path"]
            .as_str()
            .unwrap(),
    );
    assert!(
        provider.is_absolute(),
        "recovery must use generated held resources"
    );
    let nodes: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(provider).unwrap()).unwrap();
    assert_eq!(
        std::fs::read(nodes["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"backup held certificate"
    );
    assert_eq!(recovered["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(fixture.process.snapshot().pid, pid);
    drop(admission);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn merged_tun_candidate_enters_service_preparation_without_mutating_local_state() {
    let fixture =
        crate::core_session::test_support::OwnedCoreFixture::new("geodata-merged-tun-candidate")
            .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    std::fs::create_dir_all(store.root()).unwrap();
    let previous = "tun: {enable: false}\n";
    std::fs::write(store.runtime_path(), previous).unwrap();
    let pid = fixture.process.snapshot().pid;
    for payload in [
        "defaults: &base {enable: true}\ntun:\n  <<: *base\n",
        "defaults: &base {tun: {enable: true}}\n<<: *base\n",
        "a: &a {enable: true}\nb: &b {<<: *a}\ntun: {<<: *b}\n",
    ] {
        let prepared = session
            .prepare_service_profile_config(
                &store,
                crate::core_session::RuntimeConfigurationRevision {
                    binding_generation: session.runtime_descriptor().binding_generation(),
                    configuration_generation: 0,
                },
                home.join("not-committed.yaml"),
                payload.into(),
                vec![],
            )
            .await
            .unwrap();
        assert!(prepared.requires_service());
        assert_eq!(
            store.cached_runtime_payload().unwrap().as_deref(),
            Some(previous)
        );
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert_eq!(session.generation(), 0);
    }
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn directory_candidate_prepares_held_payload_before_its_destination_exists() {
    let fixture = crate::core_session::test_support::OwnedCoreFixture::new(
        "geodata-directory-final-candidate",
    )
    .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(store.runtime_path(), "tun: {enable: false}\n").unwrap();
    let destination = home.join("profiles/not-yet-committed.yaml");
    let override_path = home.join("ordered.yaml");
    std::fs::write(
        &override_path,
        "tun: {enable: true}\ndns: {enable: false, enhanced-mode: redir-host}\n",
    )
    .unwrap();
    std::fs::write(
        home.join("nodes.yaml"),
        "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
    )
    .unwrap();
    std::fs::write(home.join("cert.pem"), b"held directory certificate").unwrap();
    let held: Arc<str> =
        "tun: {enable: false}\nproxy-providers:\n  local:\n    type: file\n    path: nodes.yaml\n"
            .into();
    let prepared = session
        .prepare_service_profile_config(
            &store,
            crate::core_session::RuntimeConfigurationRevision {
                binding_generation: session.runtime_descriptor().binding_generation(),
                configuration_generation: 0,
            },
            destination.clone(),
            held,
            vec![override_path.clone()],
        )
        .await
        .unwrap();
    assert!(prepared.requires_service());
    assert!(!destination.exists());
    let PreparedConfig::Service(prepared) = prepared else {
        panic!("expected service candidate")
    };
    assert_eq!(
        prepared.next_config.as_ref().unwrap().1.profile.as_ref(),
        Some(&destination)
    );
    let yaml: serde_yaml::Value = serde_yaml::from_str(prepared.update.next_payload()).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(true));
    assert_eq!(yaml["dns"]["enable"].as_bool(), Some(false));
    std::fs::remove_file(override_path).unwrap();
    std::fs::remove_file(home.join("nodes.yaml")).unwrap();
    std::fs::remove_file(home.join("cert.pem")).unwrap();
    let bundle = &prepared.next_config.as_ref().unwrap().0;
    let recovery = ControlledConfigStore::new(home.join("held-recovery"));
    let path = bundle
        .materialize_local_runtime(&recovery, None)
        .await
        .unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let provider: serde_yaml::Value = serde_yaml::from_slice(
        &std::fs::read(yaml["proxy-providers"]["local"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(provider["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"held directory certificate"
    );
    assert_eq!(
        std::fs::read_to_string(store.runtime_path()).unwrap(),
        "tun: {enable: false}\n"
    );
    assert_eq!(session.generation(), 0);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn service_config_uses_final_overrides_and_freezes_the_exact_candidate() {
    let fixture =
        crate::core_session::test_support::OwnedCoreFixture::new("geodata-final-config-candidate")
            .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    let profile = fixture.process.launch_config().config_file.clone();
    let override_path = home.join("override.yaml");
    std::fs::write(
        &profile,
        "tun:\n  enable: false\ndns:\n  enable: false\nrules:\n- MATCH,DIRECT\n",
    )
    .unwrap();
    let expected = RuntimeConfigurationRevision {
        binding_generation: session.runtime_descriptor().binding_generation(),
        configuration_generation: 0,
    };
    assert!(
        matches!(
            session
                .prepare_service_config(
                    &store,
                    expected,
                    EffectiveConfigIntent::ActivateProfile {
                        profile: profile.clone(),
                        overrides: vec![],
                    }
                )
                .await
                .unwrap(),
            PreparedConfig::Local(_)
        ),
        "ordinary configuration must retain its checked candidate without requiring a cache"
    );
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(store.runtime_path(), std::fs::read(&profile).unwrap()).unwrap();
    std::fs::write(&override_path, "tun:\n  enable: true\nmode: global\n").unwrap();
    let prepared = session
        .prepare_service_config(
            &store,
            expected,
            EffectiveConfigIntent::Patch {
                profile: profile.clone(),
                patch: serde_json::json!({"tun":{"enable":false},"mode":"rule"}),
                overrides: vec![override_path.clone()],
            },
        )
        .await
        .unwrap();
    let PreparedConfig::Service(prepared) = prepared else {
        panic!("expected service candidate")
    };
    let (next, committed) = prepared.next_config.as_ref().unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_str(next.yaml()).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(true));
    assert_eq!(
        yaml["dns"]["enable"].as_bool(),
        Some(true),
        "fake-IP TUN must enable DNS as in the reference enhancement"
    );
    assert_eq!(yaml["mode"].as_str(), Some("global"));
    assert_eq!(committed.profile.as_ref(), Some(&profile));
    assert_eq!(committed.overrides, vec![override_path.clone()]);
    std::fs::remove_file(&profile).unwrap();
    std::fs::remove_file(&override_path).unwrap();
    assert_eq!(
        serde_yaml::from_str::<serde_yaml::Value>(prepared.update.next_payload()).unwrap()["mode"]
            .as_str(),
        Some("global")
    );
    assert_eq!(session.generation(), 0);
    assert!(fixture.process.snapshot().pid.is_some());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn tun_preparation_freezes_resources_and_releases_gates_without_stopping_local() {
    let fixture =
        crate::core_session::test_support::OwnedCoreFixture::new("geodata-tun-preparation").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ControlledConfigStore::new(home.join("controlled"));
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(home.join("cert.pem"), b"held TLS").unwrap();
    std::fs::write(
        home.join("nodes.yaml"),
        "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
    )
    .unwrap();
    std::fs::write(
        store.runtime_path(),
        "proxy-providers:\n  nodes:\n    type: file\n    path: nodes.yaml\n",
    )
    .unwrap();
    let pid = fixture.process.snapshot().pid;
    let binding = session.runtime_descriptor().binding_generation();
    std::fs::write(home.join("backup-cert.pem"), b"backup TLS").unwrap();
    std::fs::write(
        home.join("backup-nodes.yaml"),
        "proxies:\n- name: backup\n  type: http\n  certificate: backup-cert.pem\n",
    )
    .unwrap();
    *session.pending_backup.write() = Some(PendingBackupRestore {
        snapshot: CoreRestoreSnapshot {
            committed: CommittedConfig::default(),
            payload: Some(
                "proxy-providers:\n  backup:\n    type: file\n    path: backup-nodes.yaml\n".into(),
            ),
            service_bundle: None,
        },
        store_root: store.root().to_path_buf(),
        generation: 0,
    });
    let prepared = session
        .prepare_service_tun(
            &store,
            RuntimeConfigurationRevision {
                binding_generation: binding,
                configuration_generation: 0,
            },
            Some(fixture.process.launch_config().config_file.clone()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert_eq!(session.generation(), 0);
    assert!(session.transition.try_lock().is_ok());
    drop(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.lock_service_tun_mutation(),
        )
        .await
        .unwrap(),
    );
    std::fs::remove_file(home.join("cert.pem")).unwrap();
    std::fs::remove_file(home.join("nodes.yaml")).unwrap();
    std::fs::remove_file(home.join("backup-cert.pem")).unwrap();
    std::fs::remove_file(home.join("backup-nodes.yaml")).unwrap();
    let backup_bundle = prepared
        .pending_backup
        .as_ref()
        .unwrap()
        .snapshot
        .service_bundle
        .as_ref()
        .unwrap();
    let backup_store = ControlledConfigStore::new(home.join("backup-recovery"));
    let backup_config = backup_bundle
        .materialize_local_runtime(&backup_store, None)
        .await
        .unwrap();
    let backup_yaml: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(backup_config).unwrap()).unwrap();
    let backup_nodes: serde_yaml::Value = serde_yaml::from_slice(
        &std::fs::read(
            backup_yaml["proxy-providers"]["backup"]["path"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(backup_nodes["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"backup TLS"
    );
    assert!(
        session
            .prepare_service_tun(
                &store,
                RuntimeConfigurationRevision {
                    binding_generation: binding,
                    configuration_generation: 0
                },
                Some(fixture.process.launch_config().config_file.clone()),
                Some(prepared.previous.clone())
            )
            .await
            .is_err()
    );
    store
        .validate_prepared_service_tun(prepared.update.clone(), prepared.expected_cache.clone())
        .await
        .unwrap();
    let config = prepared
        .previous
        .materialize_local_runtime(&store, None)
        .await
        .unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(config).unwrap()).unwrap();
    let nodes: serde_yaml::Value = serde_yaml::from_slice(
        &std::fs::read(yaml["proxy-providers"]["nodes"]["path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(nodes["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
        b"held TLS"
    );
    std::fs::write(store.runtime_path(), "rules: [MATCH,REJECT]\n").unwrap();
    assert!(matches!(
        store
            .validate_prepared_service_tun(prepared.update.clone(), prepared.expected_cache.clone())
            .await,
        Err(ControlledConfigError::ConcurrentModification)
    ));
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert_eq!(session.generation(), 0);
    session.shutdown().await.unwrap();
}
