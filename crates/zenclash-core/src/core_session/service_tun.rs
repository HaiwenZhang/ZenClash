//! Admitted service handover; capture owns the outer completion and publication gate.

use super::*;
use crate::{owned_core::OwnedCore, service_runtime_session::ServiceRuntimeSession};

pub(crate) struct ServiceTunRuntimeOutcome {
    pub(crate) saved: Option<CoreApplyOutcome>,
    pub(crate) commit_pending: bool,
    pub(crate) recovery_warning: Option<String>,
    pub(crate) failure: Option<CoreSessionError>,
    pub(crate) restored: bool,
}

#[cfg(test)]
mod preparation_tests {
    use super::*;

    #[tokio::test]
    async fn backup_local_recovery_retains_provider_and_tls_after_sources_are_deleted() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-backup-held-recovery")
                .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ControlledConfigStore::new(home.join("controlled"));
        std::fs::create_dir_all(store.root()).unwrap();
        let old = "tun: {enable: false}\nproxy-providers:\n  held:\n    type: file\n    path: nodes.yaml\n";
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
                (session.runtime_descriptor().binding_generation(), 0),
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
            crate::core_session::ownership_tests::ChildFixture::new("geodata-merged-tun-candidate")
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
                    (session.runtime_descriptor().binding_generation(), 0),
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
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
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
            "tun: {enable: true}\ndns: {enable: false}\n",
        )
        .unwrap();
        std::fs::write(
            home.join("nodes.yaml"),
            "proxies:\n- name: held\n  type: http\n  certificate: cert.pem\n",
        )
        .unwrap();
        std::fs::write(home.join("cert.pem"), b"held directory certificate").unwrap();
        let held: Arc<str> = "tun: {enable: false}\nproxy-providers:\n  local:\n    type: file\n    path: nodes.yaml\n".into();
        let prepared = session
            .prepare_service_profile_config(
                &store,
                (session.runtime_descriptor().binding_generation(), 0),
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
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(path).unwrap()).unwrap();
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
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-final-config-candidate",
        )
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
        let expected = (session.runtime_descriptor().binding_generation(), 0);
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
            Some(false),
            "automatic admission must preserve the requested DNS setting"
        );
        assert_eq!(yaml["mode"].as_str(), Some("global"));
        assert_eq!(committed.profile.as_ref(), Some(&profile));
        assert_eq!(committed.overrides, vec![override_path.clone()]);
        std::fs::remove_file(&profile).unwrap();
        std::fs::remove_file(&override_path).unwrap();
        assert_eq!(serde_yaml::from_str::<serde_yaml::Value>(prepared.update.next_payload()).unwrap()["mode"].as_str(), Some("global"));
        assert_eq!(session.generation(), 0);
        assert!(fixture.process.snapshot().pid.is_some());
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_config_applies_checked_payload_after_sources_change_and_rejects_stale_cache() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-frozen-local-config")
                .await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ControlledConfigStore::new(home.join("controlled"));
        let profile = fixture.process.launch_config().config_file.clone();
        let overrides = home.join("override.yaml");
        std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
        std::fs::write(&overrides, "mode: direct\n").unwrap();
        let intent = EffectiveConfigIntent::ActivateProfile {
            profile: profile.clone(),
            overrides: vec![overrides.clone()],
        };
        let PreparedConfig::Local(prepared) = session
            .prepare_service_config(
                &store,
                (session.runtime_descriptor().binding_generation(), 0),
                intent.clone(),
            )
            .await
            .unwrap()
        else {
            panic!("expected Local candidate")
        };
        std::fs::write(&profile, "tun: {enable: true}\n").unwrap();
        std::fs::remove_file(&overrides).unwrap();
        let pid = fixture.process.snapshot().pid;
        let outcome = session
            .apply_prepared_local_config(&store, prepared)
            .await
            .unwrap();
        let cached: serde_yaml::Value =
            serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
        assert_eq!(cached["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(cached["mode"].as_str(), Some("direct"));
        assert_eq!(outcome.generation, 1);
        assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
        assert!(!store.root().join("override.yaml").exists());
        std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
        std::fs::write(&overrides, "mode: direct\n").unwrap();
        let PreparedConfig::Local(stale) = session
            .prepare_service_config(
                &store,
                (session.runtime_descriptor().binding_generation(), 1),
                intent,
            )
            .await
            .unwrap()
        else {
            panic!("expected Local candidate")
        };
        std::fs::write(store.runtime_path(), "mode: global\n").unwrap();
        assert!(matches!(
            session.apply_prepared_local_config(&store, stale).await,
            Err(CoreSessionError::Config(
                ControlledConfigError::ConcurrentModification
            ))
        ));
        assert_eq!(session.generation(), 1);
        assert_eq!(fixture.process.snapshot().pid, pid);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_config_restart_uses_frozen_cache_and_refuses_mutable_source_launch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for launch_cache in [false, true] {
            let fixture = crate::core_session::ownership_tests::ChildFixture::new(
                "geodata-frozen-local-restart",
            )
            .await;
            let home = fixture.process.launch_config().home_dir.clone();
            let store = ControlledConfigStore::new(home.join("controlled"));
            let profile = fixture.process.launch_config().config_file.clone();
            std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
            store.materialize(&profile).unwrap();
            let process = if launch_cache {
                fixture.process.stop_async().await.unwrap();
                let mut launch = fixture.process.launch_config().clone();
                launch.config_file = store.runtime_path();
                crate::MihomoProcess::spawn(launch).unwrap()
            } else {
                fixture.process.clone()
            };
            fixture.responder_for_test_abort();
            let address = process
                .endpoint()
                .controller
                .strip_prefix("http://")
                .unwrap();
            let listener = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match tokio::net::TcpListener::bind(address).await {
                        Ok(listener) => break listener,
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
            })
            .await
            .unwrap();
            let responder = tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let mut bytes = [0; 4096];
                    let count = stream.read(&mut bytes).await.unwrap();
                    if bytes[..count].starts_with(b"PUT ") {
                        continue;
                    }
                    let body = r#"{"meta":true,"version":"frozen-restart-fixture"}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let session = CoreSession::open(
                CoreKind::Mihomo,
                MihomoClient::from_process(process.clone()).unwrap(),
            )
            .unwrap();
            let PreparedConfig::Local(prepared) = session
                .prepare_service_config(
                    &store,
                    (session.runtime_descriptor().binding_generation(), 0),
                    EffectiveConfigIntent::Patch {
                        profile: profile.clone(),
                        patch: serde_json::json!({"mode":"direct"}),
                        overrides: vec![],
                    },
                )
                .await
                .unwrap()
            else {
                panic!("expected Local candidate")
            };
            std::fs::write(&profile, "tun: {enable: true}\nmode: global\n").unwrap();
            let pid = process.snapshot().pid;
            let outcome = session.apply_prepared_local_config(&store, prepared).await;
            let cache: serde_yaml::Value =
                serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
            assert_eq!(cache["tun"]["enable"].as_bool(), Some(false));
            if launch_cache {
                assert_eq!(outcome.unwrap().kind, CoreApplyKind::Restarted);
                assert_eq!(cache["mode"].as_str(), Some("direct"));
                assert_ne!(process.snapshot().pid, pid);
                assert_eq!(store.load_json().unwrap()["mode"], "direct");
            } else {
                assert!(outcome.is_err());
                assert_eq!(cache["mode"].as_str(), Some("rule"));
                assert_eq!(process.snapshot().pid, pid);
                assert_eq!(store.load_json().unwrap(), serde_json::json!({}));
            }
            session.shutdown().await.unwrap();
            responder.abort();
        }
    }

    #[tokio::test]
    async fn catalog_restart_reuses_final_payload_when_sources_change_after_hot_reload_attempt() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(
            "geodata-catalog-frozen-restart",
        )
        .await;
        let home = fixture.process.launch_config().home_dir.clone();
        let store = ControlledConfigStore::new(home.join("controlled"));
        let profile = fixture.process.launch_config().config_file.clone();
        let override_path = home.join("catalog-override.yaml");
        std::fs::write(&profile, "tun: {enable: false}\nmode: rule\n").unwrap();
        std::fs::write(&override_path, "mode: direct\n").unwrap();
        store
            .materialize_with_overrides(&profile, std::slice::from_ref(&override_path))
            .unwrap();
        fixture.process.stop_async().await.unwrap();
        let mut launch = fixture.process.launch_config().clone();
        launch.config_file = store.runtime_path();
        let process = crate::MihomoProcess::spawn(launch).unwrap();
        fixture.responder_for_test_abort();
        let address = process
            .endpoint()
            .controller
            .strip_prefix("http://")
            .unwrap();
        let listener = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match tokio::net::TcpListener::bind(address).await {
                    Ok(listener) => break listener,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                        tokio::task::yield_now().await
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        })
        .await
        .unwrap();
        let changed_profile = profile.clone();
        let changed_override = override_path.clone();
        let controlled_patch = store.root().join("override.yaml");
        let (observed, received) = tokio::sync::oneshot::channel();
        let responder = tokio::spawn(async move {
            let mut observed = Some(observed);
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut bytes = [0; 4096];
                    let count = stream.read(&mut bytes).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&bytes[..count]);
                }
                if request.starts_with(b"PUT ") {
                    let header_end = request
                        .windows(4)
                        .position(|bytes| bytes == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let length: usize = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .unwrap()
                        .1
                        .trim()
                        .parse()
                        .unwrap();
                    while request.len() < header_end + length {
                        let mut bytes = [0; 4096];
                        let count = stream.read(&mut bytes).await.unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&bytes[..count]);
                    }
                    let payload = serde_json::from_slice::<serde_json::Value>(
                        &request[header_end..header_end + length],
                    )
                    .unwrap()["payload"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    std::fs::write(&changed_profile, "tun: {enable: true}\nmode: global\n")
                        .unwrap();
                    std::fs::write(&changed_override, "tun: {enable: true}\nmode: global\n")
                        .unwrap();
                    std::fs::write(&controlled_patch, "tun: {enable: true}\nmode: global\n")
                        .unwrap();
                    if let Some(observed) = observed.take() {
                        observed.send(payload).unwrap();
                    }
                    continue;
                }
                let body = r#"{"meta":true,"version":"catalog-restart-fixture"}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        let pid = process.snapshot().pid;
        let staged = session
            .stage_profile_application(&store, profile.clone(), None, vec![override_path], true)
            .await
            .unwrap();
        let outcome = staged.commit(profile).await.unwrap().unwrap();
        let attempted: serde_yaml::Value = serde_yaml::from_str(&received.await.unwrap()).unwrap();
        let cached: serde_yaml::Value =
            serde_yaml::from_str(&store.cached_runtime_payload().unwrap().unwrap()).unwrap();
        assert_eq!(attempted["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(attempted["mode"].as_str(), Some("direct"));
        assert_eq!(
            cached, attempted,
            "restart must consume the final payload from the failed hot reload"
        );
        assert_eq!(outcome.kind, CoreApplyKind::Restarted);
        assert_ne!(process.snapshot().pid, pid);
        session.shutdown().await.unwrap();
        responder.abort();
    }

    #[tokio::test]
    async fn tun_preparation_freezes_resources_and_releases_gates_without_stopping_local() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-tun-preparation")
                .await;
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
                    "proxy-providers:\n  backup:\n    type: file\n    path: backup-nodes.yaml\n"
                        .into(),
                ),
                service_bundle: None,
            },
            store_root: store.root().to_path_buf(),
            generation: 0,
        });
        let prepared = session
            .prepare_service_tun(
                &store,
                binding,
                0,
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
                    binding,
                    0,
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
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(config).unwrap()).unwrap();
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
                .validate_prepared_service_tun(
                    prepared.update.clone(),
                    prepared.expected_cache.clone()
                )
                .await,
            Err(ControlledConfigError::ConcurrentModification)
        ));
        assert_eq!(fixture.process.snapshot().pid, pid);
        assert_eq!(session.generation(), 0);
        session.shutdown().await.unwrap();
    }
}

pub(crate) enum PreparedConfig {
    Local(Box<PreparedLocalConfig>),
    Service(Arc<PreparedServiceTun>),
}

impl PreparedConfig {
    pub(crate) fn requires_service(&self) -> bool {
        matches!(self, Self::Service(_))
    }

    pub(crate) fn into_profile_source(self) -> crate::profile::ProfileRuntimeSource {
        let (update, expected_cache) = match self {
            Self::Local(prepared) => (prepared.update, prepared.expected_cache),
            Self::Service(prepared) => (prepared.update.clone(), prepared.expected_cache.clone()),
        };
        crate::profile::ProfileRuntimeSource::Prepared(Box::new((update, expected_cache)))
    }
}

pub(crate) struct PreparedLocalConfig {
    update: crate::ControlledConfigUpdate,
    expected_cache: Option<String>,
    binding: u64,
    generation: u64,
    session_generation: Arc<AtomicU64>,
    store_root: PathBuf,
    committed: CommittedConfig,
    persist_patch: bool,
}

#[derive(Clone)]
pub(crate) struct PreparedServiceTun {
    update: crate::ControlledConfigUpdate,
    previous: Arc<crate::ServiceRuntimeBundle>,
    expected_cache: Option<String>,
    binding: u64,
    generation: u64,
    session_generation: Arc<AtomicU64>,
    pending_backup: Option<PendingBackupRestore>,
    expected_pending: Option<(u64, PathBuf)>,
    next_config: Option<(Arc<crate::ServiceRuntimeBundle>, CommittedConfig)>,
    store_root: PathBuf,
    pub(crate) profile_commit: Option<Arc<crate::profiles::application::ServiceProfileCommit>>,
    pub(crate) authorized_store: Option<ControlledConfigStore>,
}

impl PreparedServiceTun {
    pub(crate) fn expected_runtime(&self) -> (u64, u64) {
        (self.binding, self.generation)
    }
}

impl CoreSession {
    pub(crate) async fn prepare_service_backup_config(
        &self,
        store: &ControlledConfigStore,
        expected: (u64, u64),
        candidate: &crate::backup::BackupRuntimeCandidate,
    ) -> Result<(PreparedConfig, CoreRestoreSnapshot), CoreSessionError> {
        let client = self.client.pin_binding()?;
        self.check_service_tun_admission(&client, expected.0, expected.1)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(MihomoError::StaleBinding.into()),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let _transition = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.check_service_tun_admission(&client, expected.0, expected.1)?;
        let read = store.clone();
        let session = self.clone();
        let worker_candidate = candidate.clone();
        let (update, mut previous) = tokio::task::spawn_blocking(move || {
            let previous = session.capture_restore_snapshot(&read)?;
            let payload = previous.payload.clone().ok_or_else(|| {
                ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                ))
            })?;
            let update = read.prepare_backup_config_update(&worker_candidate, payload)?;
            Ok::<_, CoreSessionError>((update, previous))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let committed = CommittedConfig {
            profile: Some(candidate.profile.clone()),
            overrides: candidate.overrides.clone(),
        };
        let previous_bundle = Arc::new(
            crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone()).await?,
        );
        previous.service_bundle = Some(previous_bundle.clone());
        let prepared = if crate::tun_admission::yaml_enables_tun(update.next_payload())? {
            let next =
                Arc::new(crate::ServiceRuntimeBundle::prepare(update.next_payload(), home).await?);
            PreparedConfig::Service(Arc::new(PreparedServiceTun {
                update,
                previous: previous_bundle,
                expected_cache: None,
                binding: expected.0,
                generation: expected.1,
                session_generation: self.generation.clone(),
                pending_backup: None,
                expected_pending: self.service_tun_pending_identity(),
                next_config: Some((next, committed)),
                store_root: store.root().to_path_buf(),
                profile_commit: None,
                authorized_store: None,
            }))
        } else {
            PreparedConfig::Local(Box::new(PreparedLocalConfig {
                update,
                expected_cache: None,
                binding: expected.0,
                generation: expected.1,
                session_generation: self.generation.clone(),
                store_root: store.root().to_path_buf(),
                committed,
                persist_patch: false,
            }))
        };
        self.check_service_tun_admission(&client, expected.0, expected.1)?;
        Ok((prepared, previous))
    }

    pub(crate) fn validate_backup_snapshot(
        expected: &CoreRestoreSnapshot,
        actual: &CoreRestoreSnapshot,
    ) -> Result<(), CoreSessionError> {
        if expected.payload != actual.payload
            || expected.committed.profile != actual.committed.profile
            || expected.committed.overrides != actual.committed.overrides
        {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }
    pub(crate) async fn prepare_service_tun(
        &self,
        store: &ControlledConfigStore,
        binding: u64,
        generation: u64,
        fallback_profile: Option<PathBuf>,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
    ) -> Result<Arc<PreparedServiceTun>, CoreSessionError> {
        let client = self.client.pin_binding()?;
        self.check_service_tun_admission(&client, binding, generation)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let committed = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.check_service_tun_admission(&client, binding, generation)?;
        let profile = committed
            .profile
            .clone()
            .or(fallback_profile)
            .ok_or(CoreSessionError::NoCommittedProfile)?;
        let read = store.clone();
        let expected_cache = tokio::task::spawn_blocking(move || read.cached_runtime_payload())
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let update = if let Some(bundle) = recovery_bundle.as_ref() {
            store
                .prepare_service_tun_recovery_update(bundle.clone())
                .await?
        } else {
            store
                .prepare_service_tun_update(profile, committed.overrides.clone(), None)
                .await?
        };
        let previous = match recovery_bundle {
            Some(bundle) => bundle,
            None => Arc::new(
                crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone())
                    .await?,
            ),
        };
        let delta = serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}});
        self.validate_backup_delta(&delta)?;
        let expected_pending = self.service_tun_pending_identity();
        let pending_backup = self
            .prepare_service_tun_backup(&home, update.previous_payload(), &previous, &delta)
            .await?;
        if self.service_tun_pending_identity() != expected_pending {
            return Err(MihomoError::StaleBinding.into());
        }
        store
            .validate_prepared_service_tun(update.clone(), expected_cache.clone())
            .await?;
        self.check_service_tun_admission(&client, binding, generation)?;
        Ok(Arc::new(PreparedServiceTun {
            update,
            previous,
            expected_cache,
            binding,
            generation,
            session_generation: self.generation.clone(),
            pending_backup,
            expected_pending,
            next_config: None,
            store_root: store.root().to_path_buf(),
            profile_commit: None,
            authorized_store: None,
        }))
    }

    pub(crate) async fn prepare_service_config(
        &self,
        store: &ControlledConfigStore,
        expected: (u64, u64),
        intent: EffectiveConfigIntent,
    ) -> Result<PreparedConfig, CoreSessionError> {
        self.prepare_service_config_source(store, expected, intent, None)
            .await
    }

    pub(crate) async fn prepare_service_profile_config(
        &self,
        store: &ControlledConfigStore,
        expected: (u64, u64),
        profile: PathBuf,
        payload: Arc<str>,
        overrides: Vec<PathBuf>,
    ) -> Result<PreparedConfig, CoreSessionError> {
        self.prepare_service_config_source(
            store,
            expected,
            EffectiveConfigIntent::ActivateProfile { profile, overrides },
            Some(crate::profile::ProfileRuntimeSource::Frozen(payload)),
        )
        .await
    }

    async fn prepare_service_config_source(
        &self,
        store: &ControlledConfigStore,
        expected: (u64, u64),
        intent: EffectiveConfigIntent,
        source: Option<crate::profile::ProfileRuntimeSource>,
    ) -> Result<PreparedConfig, CoreSessionError> {
        let (binding, generation) = expected;
        let client = self.client.pin_binding()?;
        self.check_service_tun_admission(&client, binding, generation)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let committed = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.check_service_tun_admission(&client, binding, generation)?;
        let (profile, overrides, patch) = match intent {
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                (profile, overrides, None)
            }
            EffectiveConfigIntent::ReapplyCurrent { overrides } => (
                committed
                    .profile
                    .clone()
                    .ok_or(CoreSessionError::NoCommittedProfile)?,
                overrides,
                None,
            ),
            EffectiveConfigIntent::Patch {
                profile,
                overrides,
                patch,
            } => (
                committed.profile.clone().unwrap_or(profile),
                if committed.profile.is_some() {
                    committed.overrides.clone()
                } else {
                    overrides
                },
                Some(patch),
            ),
        };
        let read = store.clone();
        let expected_cache = tokio::task::spawn_blocking(move || read.cached_runtime_payload())
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let persist_patch = patch.is_some();
        let update = store
            .prepare_service_config_update(
                source.unwrap_or_else(|| profile.clone().into()),
                patch,
                overrides.clone(),
                expected_cache.clone(),
            )
            .await?;
        if !crate::tun_admission::yaml_enables_tun(update.next_payload())? {
            store
                .validate_prepared_service_tun(update.clone(), expected_cache.clone())
                .await?;
            self.check_service_tun_admission(&client, binding, generation)?;
            return Ok(PreparedConfig::Local(Box::new(PreparedLocalConfig {
                update,
                expected_cache,
                binding,
                generation,
                session_generation: self.generation.clone(),
                store_root: store.root().to_path_buf(),
                committed: CommittedConfig {
                    profile: Some(profile),
                    overrides,
                },
                persist_patch,
            })));
        }
        if expected_cache.is_none() {
            return Err(ControlledConfigError::Transaction(zenclash_i18n::text(
                "core_page.service.no_snapshot",
            ))
            .into());
        }
        let previous = Arc::new(
            crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone()).await?,
        );
        let next =
            Arc::new(crate::ServiceRuntimeBundle::prepare(update.next_payload(), home).await?);
        let expected_pending = self.service_tun_pending_identity();
        store
            .validate_prepared_service_tun(update.clone(), expected_cache.clone())
            .await?;
        self.check_service_tun_admission(&client, binding, generation)?;
        Ok(PreparedConfig::Service(Arc::new(PreparedServiceTun {
            update,
            previous,
            expected_cache,
            binding,
            generation,
            session_generation: self.generation.clone(),
            pending_backup: None,
            expected_pending,
            next_config: Some((
                next,
                CommittedConfig {
                    profile: Some(profile),
                    overrides,
                },
            )),
            store_root: store.root().to_path_buf(),
            profile_commit: None,
            authorized_store: None,
        })))
    }

    pub(crate) async fn apply_prepared_local_config(
        &self,
        store: &ControlledConfigStore,
        prepared: Box<PreparedLocalConfig>,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let prepared = *prepared;
        if prepared.store_root != store.root()
            || !Arc::ptr_eq(&prepared.session_generation, &self.generation)
        {
            return Err(MihomoError::StaleBinding.into());
        }
        let client = self.client.pin_binding()?;
        self.check_service_tun_admission(&client, prepared.binding, prepared.generation)?;
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mut committed = self.transition.clone().lock_owned().await;
        self.check_service_tun_admission(&client, prepared.binding, prepared.generation)?;
        let session = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let (kind, receipt) = store
                .apply_frozen_local_update(
                    &client,
                    prepared.update,
                    prepared.expected_cache,
                    prepared.persist_patch,
                    session.shutdown_requested.clone(),
                )
                .await
                .map_err(|error| session.runtime_mutation_error(error))?;
            *committed = prepared.committed.clone();
            let generation = session.next_generation_with_config(Some(prepared.committed));
            receipt.confirmation?;
            Ok(CoreApplyOutcome { kind, generation })
        })
        .await
        .map_err(|error| {
            self.mark_runtime_unknown();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    pub(crate) async fn enable_service_tun_admitted(
        &self,
        store: &ControlledConfigStore,
        service: Option<(Arc<zenclash_service::ServiceClient>, PathBuf)>,
        expected_runtime: (u64, u64),
        fallback_profile: Option<PathBuf>,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        prepared_config: Option<Arc<PreparedServiceTun>>,
    ) -> Result<ServiceTunRuntimeOutcome, CoreSessionError> {
        let (expected_binding, expected_generation) = expected_runtime;
        let before = self.client.pin_binding()?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let old = before
            .owned_core()
            .ok_or(CoreSessionError::ReleaseUnsupported { core: self.kind })?;
        let source_home = match (&old, &service) {
            (OwnedCore::Local(process), Some((_, home)))
                if process.launch_config().home_dir == *home =>
            {
                home.clone()
            }
            (OwnedCore::Service(runtime), None) => runtime.source_home().to_path_buf(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let mut scopes = before.write_scopes();
        scopes.push(source_home.clone());
        let profile_commit = prepared_config
            .as_ref()
            .and_then(|prepared| prepared.profile_commit.clone());
        if let Some(profile) = &profile_commit {
            scopes.push(profile.root().to_path_buf());
        }
        let lease = store.acquire_write_lease_for_paths(scopes).await?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let store = store.with_write_lease(&lease);
        let mut committed = self.transition.clone().lock_owned().await;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let _store_mutation = store.lock_service_tun_mutation().await;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let mut mutation = before.lock_runtime_binding().await?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let result = async {
            if let Some(profile) = &profile_commit {
                profile.validate(&lease).await?;
            }
            let next_config = prepared_config
                .as_ref()
                .and_then(|prepared| prepared.next_config.clone());
            let prepared_config_is_full = next_config.is_some();
            let export_local_caches = recovery_bundle.is_some();
            let profile = next_config
                .as_ref()
                .and_then(|(_, config)| config.profile.clone())
                .or_else(|| committed.profile.clone())
                .or(fallback_profile)
                .ok_or(CoreSessionError::NoCommittedProfile)?;
            let held = match &old {
                OwnedCore::Service(runtime) if recovery_bundle.is_none() => {
                    Some(runtime.snapshot()?)
                }
                OwnedCore::Service(_) => return Err(MihomoError::StaleBinding.into()),
                OwnedCore::Local(_) => recovery_bundle.clone(),
            };
            let update = if let Some(prepared) = prepared_config.as_ref() {
                if prepared.store_root != store.root()
                    || !Arc::ptr_eq(&prepared.session_generation, &self.generation)
                    || prepared.binding != expected_binding
                    || prepared.generation != expected_generation
                {
                    return Err(MihomoError::StaleBinding.into());
                }
                store
                    .validate_prepared_service_tun(
                        prepared.update.clone(),
                        prepared.expected_cache.clone(),
                    )
                    .await?;
                prepared.update.clone()
            } else if let Some(bundle) = recovery_bundle {
                store.prepare_service_tun_recovery_update(bundle).await?
            } else {
                store
                    .prepare_service_tun_update(
                        profile.clone(),
                        committed.overrides.clone(),
                        held.clone(),
                    )
                    .await?
            };
            let previous_bundle = match (prepared_config.as_ref(), held) {
                (Some(prepared), _) => prepared.previous.clone(),
                (None, Some(bundle)) => bundle,
                (None, None) => Arc::new(
                    crate::ServiceRuntimeBundle::prepare(
                        update.previous_payload(),
                        source_home.clone(),
                    )
                    .await?,
                ),
            };
            let delta = serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}});
            if !prepared_config_is_full {
                self.validate_backup_delta(&delta)?;
            }
            let pending_backup = if let Some(prepared) = prepared_config {
                if self.service_tun_pending_identity() != prepared.expected_pending {
                    return Err(MihomoError::StaleBinding.into());
                }
                prepared.pending_backup.clone()
            } else {
                self.prepare_service_tun_backup(
                    &source_home,
                    update.previous_payload(),
                    &previous_bundle,
                    &delta,
                )
                .await?
            };
            let next_bundle = if let Some((bundle, _)) = next_config.as_ref() {
                bundle.clone()
            } else {
                Arc::new(previous_bundle.with_delta(&delta)?)
            };
            let runtime = match (&old, service) {
                (OwnedCore::Service(runtime), None) => runtime.clone(),
                (OwnedCore::Local(process), Some((client, _))) => {
                    ServiceRuntimeSession::from_local(client, process.launch_config().clone())
                }
                _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
            };
            let mut prepared = runtime.prepare_bundle(next_bundle).await?;
            let persistence = store.stage_service_tun_update(update).await?;
            if let Some(profile) = &profile_commit {
                profile.mark_runtime_attempted();
            }
            if let OwnedCore::Local(process) = &old {
                // A is reaped before B can start. The exact B owner is published before Start,
                // so an uncertain acknowledgement cannot hide a kernel from shutdown.
                if let Err(error) = process.stop_async().await {
                    drop(prepared);
                    let _cache = persistence.rollback().await;
                    let _release = runtime.release_owned().await;
                    return Ok(self.failed_service_tun(error.into(), false));
                }
                if export_local_caches {
                    // The stopped recovery slot is authoritative for downloaded HTTP caches.
                    // Restage before Start; TLS and GeoData remain the held accepted bytes.
                    drop(prepared);
                    let refreshed = async {
                        let bundle = previous_bundle
                            .export_local_caches(
                                &store,
                                process.launch_config().config_file.clone(),
                            )
                            .await?;
                        let bundle = Arc::new(bundle.with_delta(&delta)?);
                        runtime.prepare_bundle(bundle).await
                    }
                    .await;
                    prepared = match refreshed {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let cache = persistence.rollback().await;
                            let restored = self
                                .recover_service_tun(
                                    &old,
                                    &runtime,
                                    (&store, _store_mutation),
                                    &previous_bundle,
                                    mutation,
                                    cache.is_ok(),
                                )
                                .await;
                            return Ok(self.failed_service_tun(error.into(), restored));
                        }
                    };
                }
                if let Err(error) = self.ensure_not_shutting_down() {
                    drop(prepared);
                    let cache = persistence.rollback().await;
                    let recovered = self
                        .recover_service_tun(
                            &old,
                            &runtime,
                            (&store, _store_mutation),
                            &previous_bundle,
                            mutation,
                            cache.is_ok(),
                        )
                        .await;
                    return Ok(self.failed_service_tun(error, recovered));
                }
                match self
                    .client
                    .publish_prepared_service(runtime.clone(), mutation)
                    .await
                {
                    Ok(guard) => mutation = guard,
                    Err(error) => {
                        drop(prepared);
                        let cache = persistence.rollback().await;
                        // Publication may have succeeded before retirement reported a failure.
                        // Reacquire the current mutation gate before inspecting or restoring it.
                        let recovered = match self.client.lock_runtime_binding().await {
                            Ok(guard) => {
                                self.recover_service_tun(
                                    &old,
                                    &runtime,
                                    (&store, _store_mutation),
                                    &previous_bundle,
                                    guard,
                                    cache.is_ok(),
                                )
                                .await
                            }
                            Err(_) => {
                                let _release = runtime.release_owned().await;
                                false
                            }
                        };
                        return Ok(self.failed_service_tun(error.into(), recovered));
                    }
                }
            }
            let applied = prepared.apply(true).await;
            let applied = match applied {
                Ok(applied) => applied,
                Err(error) => {
                    let cache = persistence.rollback().await;
                    let recovered = self
                        .recover_service_tun(
                            &old,
                            &runtime,
                            (&store, _store_mutation),
                            &previous_bundle,
                            mutation,
                            cache.is_ok(),
                        )
                        .await;
                    return Ok(self.failed_service_tun(error.into(), recovered));
                }
            };
            let live = match self.client.pin_binding() {
                Ok(client) => client.runtime_config().await,
                Err(error) => Err(error),
            };
            let verified = live.as_ref().is_ok_and(|config| config.tun.enable);
            if !verified || self.is_shutting_down() {
                drop(applied);
                let cache = persistence.rollback().await;
                let recovered = self
                    .recover_service_tun(
                        &old,
                        &runtime,
                        (&store, _store_mutation),
                        &previous_bundle,
                        mutation,
                        cache.is_ok(),
                    )
                    .await;
                return Ok(self.failed_service_tun(
                    live.err().map(CoreSessionError::from).unwrap_or_else(|| {
                        CoreSessionError::Process(MihomoError::Process(zenclash_i18n::text(
                            "core_page.service.unknown",
                        )))
                    }),
                    recovered,
                ));
            }
            let save = if let Some(profile) = &profile_commit {
                // A directory application must not rewrite the controlled layer.
                // Both writes remain rollbackable until the catalog compare-commit succeeds.
                match persistence.validate_patch().await {
                    Ok(()) => profile.commit(&lease).await,
                    Err(error) => Err(error.into()),
                }
            } else {
                persistence.save().await.map_err(CoreSessionError::from)
            };
            if let Err(error) = save {
                drop(applied);
                let cache = persistence.rollback().await;
                let recovered = self
                    .recover_service_tun(
                        &old,
                        &runtime,
                        (&store, _store_mutation),
                        &previous_bundle,
                        mutation,
                        cache.is_ok(),
                    )
                    .await;
                return Ok(self.failed_service_tun(error, recovered));
            }
            persistence.saved();
            let (generation, pending_conflict) = if let Some((_, next_committed)) = next_config {
                *committed = next_committed;
                (
                    self.next_generation_with_config(Some(committed.clone())),
                    false,
                )
            } else {
                committed.profile = Some(profile);
                self.accept_saved_service_tun(committed.clone(), pending_backup)
            };
            let saved = CoreApplyOutcome {
                kind: if prepared_config_is_full {
                    CoreApplyKind::HotReloaded
                } else {
                    CoreApplyKind::Patched
                },
                generation,
            };
            // Once the override is durable, retain its receipt and finish the same native candidate.
            // Even an in-memory publication error must not drop a saved candidate back to Applied.
            let confirmation = applied.commit().await.map_err(CoreSessionError::from);
            let commit_pending = confirmation.is_err();
            let recovery_warning =
                pending_conflict.then(|| MihomoError::StaleTransport.to_string());
            let failure = pending_conflict
                .then(|| CoreSessionError::Process(MihomoError::StaleTransport))
                .or_else(|| confirmation.err());
            drop(mutation);
            self.lifecycle.write().phase = if failure.is_none() {
                CoreLifecyclePhase::Stable
            } else {
                CoreLifecyclePhase::Unknown
            };
            Ok(ServiceTunRuntimeOutcome {
                saved: Some(saved),
                commit_pending,
                recovery_warning,
                failure,
                restored: false,
            })
        }
        .await;
        if let Some(profile) = &profile_commit {
            // Capture still owns publication, so this is this transaction's version.
            profile.record_runtime_generation(self.generation());
        }
        // A stopped local owner may be the last Arc once B is published. Retire
        // both old pins away from async workers while transition/capture still own admission.
        let retirement = tokio::task::spawn_blocking(move || drop((old, before)))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()));
        self.finish_service_tun_retirement(result, retirement)
    }

    fn finish_service_tun_retirement(
        &self,
        result: Result<ServiceTunRuntimeOutcome, CoreSessionError>,
        retirement: Result<(), ControlledConfigError>,
    ) -> Result<ServiceTunRuntimeOutcome, CoreSessionError> {
        let Err(error) = retirement else {
            return result;
        };
        tracing::warn!(%error, "failed to retire previous core binding after service TUN completion");
        self.lifecycle.write().phase = CoreLifecyclePhase::Unknown;
        result.map(|mut outcome| {
            let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
            if let Some(previous) = &mut outcome.recovery_warning {
                previous.push_str("; ");
                previous.push_str(&warning);
            } else {
                outcome.recovery_warning = Some(warning);
            }
            if outcome.failure.is_none() {
                outcome.failure = Some(CoreSessionError::PreviousCoreCleanupUnconfirmed);
            }
            outcome.restored = false;
            outcome
        })
    }

    async fn prepare_service_tun_backup(
        &self,
        home: &std::path::Path,
        previous_payload: &str,
        bundle: &Arc<crate::ServiceRuntimeBundle>,
        delta: &serde_json::Value,
    ) -> Result<Option<PendingBackupRestore>, CoreSessionError> {
        let Some(mut pending) = self.pending_backup.read().clone() else {
            return Ok(None);
        };
        if pending.snapshot.service_bundle.is_none() {
            let payload = pending.snapshot.payload.as_deref().ok_or_else(|| {
                ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                ))
            })?;
            pending.snapshot.service_bundle = Some(if payload == previous_payload {
                bundle.clone()
            } else {
                Arc::new(crate::ServiceRuntimeBundle::prepare(payload, home.to_path_buf()).await?)
            });
        }
        pending.snapshot = snapshot_with_delta(&pending.snapshot, delta)?;
        Ok(Some(pending))
    }

    fn service_tun_pending_identity(&self) -> Option<(u64, PathBuf)> {
        self.pending_backup
            .read()
            .as_ref()
            .map(|pending| (pending.generation, pending.store_root.clone()))
    }

    fn accept_saved_service_tun(
        &self,
        config: CommittedConfig,
        prepared: Option<PendingBackupRestore>,
    ) -> (u64, bool) {
        // Every fallible resource/YAML operation was completed before A stopped.
        // Publication of the durable receipt must never roll back the saved cache.
        let mut committed = self.committed_profile.write();
        let mut pending = self.pending_backup.write();
        let matches = match (pending.as_ref(), prepared.as_ref()) {
            (None, None) => true,
            (Some(current), Some(prepared)) => {
                current.generation == prepared.generation
                    && current.store_root == prepared.store_root
            }
            _ => false,
        };
        if matches {
            *pending = prepared;
        }
        committed.config = config;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        committed.generation = generation;
        if let Some(pending) = pending.as_mut() {
            pending.generation = generation;
        }
        self.client.invalidate_connections();
        (generation, !matches)
    }

    fn check_service_tun_admission(
        &self,
        client: &MihomoClient,
        binding: u64,
        generation: u64,
    ) -> Result<(), CoreSessionError> {
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        if self.kind != CoreKind::Mihomo
            || self.runtime_descriptor().binding_generation() != binding
            || self.generation() != generation
        {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }

    async fn recover_service_tun(
        &self,
        old: &OwnedCore,
        runtime: &Arc<ServiceRuntimeSession>,
        admission: (&ControlledConfigStore, tokio::sync::OwnedMutexGuard<()>),
        previous_bundle: &Arc<crate::ServiceRuntimeBundle>,
        mutation: tokio::sync::OwnedMutexGuard<()>,
        cache_restored: bool,
    ) -> bool {
        let (store, store_mutation) = admission;
        if let OwnedCore::Local(process) = old {
            // Release includes confirmed Stop. An unknown result keeps B bound;
            // restoring A would risk two simultaneous TUN kernels.
            if runtime.release_owned().await.is_err() || !cache_restored || self.is_shutting_down()
            {
                return false;
            }
            let client = self.client.clone();
            runtime
                .recover_held_local_runtime(
                    (store, store_mutation),
                    process.launch_config().clone(),
                    previous_bundle.clone(),
                    self.shutdown_requested.clone(),
                    CORE_READY_TIMEOUT,
                    move |process| async move {
                        client.publish_prepared_process(process, mutation).await
                    },
                )
                .await
                .is_ok_and(|outcome| outcome.failure.is_none())
        } else {
            let _mutation = mutation;
            cache_restored && runtime.restore_active().await.is_ok()
        }
    }

    fn failed_service_tun(
        &self,
        failure: CoreSessionError,
        restored: bool,
    ) -> ServiceTunRuntimeOutcome {
        self.next_generation();
        self.lifecycle.write().phase = if restored {
            CoreLifecyclePhase::Stable
        } else {
            CoreLifecyclePhase::Unknown
        };
        ServiceTunRuntimeOutcome {
            saved: None,
            commit_pending: false,
            recovery_warning: None,
            failure: Some(failure),
            restored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn service_tun_retirement_failure_keeps_saved_receipt_and_current_owner() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let binding = session.runtime_descriptor().binding_generation();
        let (generation, _) = session.accept_saved_service_tun(CommittedConfig::default(), None);
        let receipt = CoreApplyOutcome {
            kind: CoreApplyKind::Patched,
            generation,
        };
        let result = session.finish_service_tun_retirement(
            Ok(ServiceTunRuntimeOutcome {
                saved: Some(receipt),
                commit_pending: false,
                recovery_warning: None,
                failure: None,
                restored: false,
            }),
            Err(ControlledConfigError::Task(
                "retirement worker failed".into(),
            )),
        );
        assert!(result.is_ok(), "durable receipt became an ordinary error");
        let outcome = result.ok().unwrap();
        assert_eq!(outcome.saved, Some(receipt));
        assert!(!outcome.commit_pending);
        assert_eq!(
            outcome.recovery_warning,
            Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
        );
        assert_eq!(
            outcome.failure.unwrap().to_string(),
            zenclash_i18n::text("core_page.service.cleanup_unconfirmed")
        );
        assert!(!outcome.restored);
        assert_eq!(session.generation(), generation);
        assert_eq!(session.lifecycle.read().phase, CoreLifecyclePhase::Unknown);
        assert_eq!(session.runtime_descriptor().binding_generation(), binding);
    }

    #[tokio::test]
    async fn service_tun_retirement_warning_is_independent_of_pending_commit() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let outcome = session
            .finish_service_tun_retirement(
                Ok(ServiceTunRuntimeOutcome {
                    saved: Some(CoreApplyOutcome {
                        kind: CoreApplyKind::Patched,
                        generation: 1,
                    }),
                    commit_pending: true,
                    recovery_warning: None,
                    failure: Some(MihomoError::StaleTransport.into()),
                    restored: false,
                }),
                Err(ControlledConfigError::Task(
                    "retirement worker failed".into(),
                )),
            )
            .unwrap();
        assert!(outcome.commit_pending);
        assert_eq!(
            outcome.recovery_warning,
            Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
        );
        assert!(matches!(
            outcome.failure,
            Some(CoreSessionError::Process(MihomoError::StaleTransport))
        ));
    }

    #[tokio::test]
    async fn service_tun_retirement_failure_preserves_original_unsaved_error() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let result = session.finish_service_tun_retirement(
            Err(MihomoError::StaleBinding.into()),
            Err(ControlledConfigError::Task(
                "retirement worker failed".into(),
            )),
        );
        assert!(matches!(
            result,
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
    }

    #[tokio::test]
    async fn pending_service_tun_target_freezes_before_deletion_and_changes_only_after_save() {
        let home = std::env::temp_dir().join(format!(
            "zenclash-tun-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("rules.yaml"), "payload: [example.com]\n").unwrap();
        let payload = "mode: rule\ntun: {enable: false}\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: rules.yaml\n";
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        *session.pending_backup.write() = Some(PendingBackupRestore {
            snapshot: CoreRestoreSnapshot {
                committed: CommittedConfig::default(),
                payload: Some(payload.into()),
                service_bundle: None,
            },
            store_root: home.clone(),
            generation: 0,
        });
        let bundle = Arc::new(
            crate::ServiceRuntimeBundle::prepare(payload, home.clone())
                .await
                .unwrap(),
        );
        std::fs::remove_file(home.join("rules.yaml")).unwrap();
        let delta = serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}});
        let prepared = session
            .prepare_service_tun_backup(&home, payload, &bundle, &delta)
            .await
            .unwrap();
        assert!(
            session
                .pending_backup
                .read()
                .as_ref()
                .unwrap()
                .snapshot
                .service_bundle
                .is_none()
        );
        assert!(
            session
                .pending_backup
                .read()
                .as_ref()
                .unwrap()
                .snapshot
                .payload
                .as_ref()
                .unwrap()
                .contains("enable: false")
        );
        let (generation, conflict) =
            session.accept_saved_service_tun(CommittedConfig::default(), prepared);
        assert_eq!(generation, 1);
        assert!(!conflict);
        let pending = session.pending_backup.read().clone().unwrap();
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(pending.snapshot.payload.as_ref().unwrap()).unwrap();
        assert_eq!(yaml["tun"]["enable"], true);
        assert!(
            pending
                .snapshot
                .service_bundle
                .unwrap()
                .yaml()
                .contains("assets/")
        );
        std::fs::remove_dir(&home).unwrap();
    }

    #[tokio::test]
    async fn later_pending_target_is_preserved_when_saved_publication_detects_conflict() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let pending = PendingBackupRestore {
            snapshot: CoreRestoreSnapshot {
                committed: CommittedConfig::default(),
                payload: Some("mode: rule\n".into()),
                service_bundle: None,
            },
            store_root: PathBuf::from("pending-root"),
            generation: 0,
        };
        *session.pending_backup.write() = Some(pending.clone());
        session.pending_backup.write().as_mut().unwrap().generation = 1;
        session
            .pending_backup
            .write()
            .as_mut()
            .unwrap()
            .snapshot
            .payload = Some("mode: global\n".into());
        let (generation, conflict) =
            session.accept_saved_service_tun(CommittedConfig::default(), Some(pending));
        assert!(conflict);
        let current = session.pending_backup.read().clone().unwrap();
        assert_eq!(current.snapshot.payload.as_deref(), Some("mode: global\n"));
        assert_eq!(current.generation, generation);
    }
}
