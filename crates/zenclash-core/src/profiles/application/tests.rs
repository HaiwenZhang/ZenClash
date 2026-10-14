use std::{
    fs,
    io::{BufReader, Read, Write},
    net::TcpListener,
    thread,
};

use crate::{CoreKind, MihomoClient, MihomoEndpoint};

use super::*;

#[tokio::test]
async fn service_directory_stale_rejection_does_not_claim_a_newer_runtime_failure() {
    let fixture = Fixture::new("service-directory-stale-outcome");
    let application = fixture.application("127.0.0.1:9".parse().unwrap());
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let recovery = ProfileRecovery {
        attempted: ProfileSourceMetadata::from(prepared.source_candidate.record()),
        last_known_good: prepared.source_candidate.last_known_good(),
    };
    let participant = ServiceProfileCommit::new(prepared.source_candidate);
    let newer = application.session.invalidate_runtime_observations();
    let result = participant.unsaved_outcome(
        &application.session,
        recovery,
        Box::new(crate::ServiceManagerError::Stale).into(),
        false,
    );
    assert!(matches!(
        result,
        ProfileApplyOutcome::Rejected {
            cause: ProfileApplicationError::Service(_),
            ..
        }
    ));
    assert_eq!(application.session.generation(), newer);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn service_directory_failure_keeps_its_own_completed_runtime_version() {
    let fixture = Fixture::new("service-directory-failure-version");
    let application = fixture.application("127.0.0.1:9".parse().unwrap());
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let recovery = ProfileRecovery {
        attempted: ProfileSourceMetadata::from(prepared.source_candidate.record()),
        last_known_good: prepared.source_candidate.last_known_good(),
    };
    let participant = ServiceProfileCommit::new(prepared.source_candidate);
    participant.mark_runtime_attempted();
    let completed = application.session.invalidate_runtime_observations();
    participant.record_runtime_generation(completed);
    let newer = application.session.invalidate_runtime_observations();
    let result = participant.unsaved_outcome(
        &application.session,
        recovery,
        ProfileApplicationError::Task("trial failed".into()),
        false,
    );
    assert!(
        matches!(result, ProfileApplyOutcome::RuntimeUnknown { runtime_version, .. } if runtime_version == completed)
    );
    assert_eq!(application.session.generation(), newer);
}

#[tokio::test]
async fn service_catalog_participant_defers_commit_and_keeps_held_source() {
    let fixture = Fixture::new("service-catalog-held");
    let application = fixture.application("127.0.0.1:9".parse().unwrap());
    let source = fixture.write_source("new.yaml", "HELD");
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source: source.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let record = prepared.source_candidate.record().clone();
    let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.source_candidate));
    fs::remove_file(&source).unwrap();
    let lease = fixture
        .controlled
        .acquire_write_lease_for_paths(vec![fixture.store.root().to_path_buf()])
        .await
        .unwrap();
    participant.validate(&lease).await.unwrap();
    assert!(!fixture.store.profile_path(&record).exists());
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    participant.commit(&lease).await.unwrap();
    assert_eq!(
        fs::read_to_string(fixture.store.profile_path(&record)).unwrap(),
        profile_payload("HELD")
    );
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(record.id.as_str())
    );
    assert!(participant.committed.lock().is_some());
    assert!(!source.exists());
    assert!(
        participant.commit(&lease).await.is_err(),
        "the receipt cannot be committed twice"
    );
}

#[tokio::test]
async fn service_catalog_conflict_restores_staged_cache_without_rewriting_controlled_layer() {
    let fixture = Fixture::new("service-catalog-conflict");
    let application = fixture.application("127.0.0.1:9".parse().unwrap());
    let source = fixture.write_source("new.yaml", "HELD");
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source,
            overrides: vec![],
        })
        .await
        .unwrap();
    let destination = fixture
        .store
        .profile_path(prepared.source_candidate.record());
    let payload = prepared.source_candidate.payload().clone();
    let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.source_candidate));
    let lease = fixture
        .controlled
        .acquire_write_lease_for_paths(vec![fixture.store.root().to_path_buf()])
        .await
        .unwrap();
    let controlled = fixture.controlled.with_write_lease(&lease);
    fs::create_dir_all(controlled.root()).unwrap();
    fs::write(controlled.runtime_path(), "mode: direct\n").unwrap();
    let update = controlled
        .prepare_service_config_update(
            crate::profile::ProfileRuntimeSource::Frozen(payload),
            None,
            vec![],
            Some("mode: direct\n".into()),
        )
        .await
        .unwrap();
    participant.validate(&lease).await.unwrap();
    let persistence = controlled.stage_service_tun_update(update).await.unwrap();
    persistence.validate_patch().await.unwrap();
    fixture.store.activate(&fixture.candidate.id).unwrap();
    assert!(participant.commit(&lease).await.is_err());
    persistence.rollback().await.unwrap();
    assert_eq!(
        fs::read_to_string(controlled.runtime_path()).unwrap(),
        "mode: direct\n"
    );
    assert!(!controlled.root().join("override.yaml").exists());
    assert!(!destination.exists());
    assert!(participant.committed.lock().is_none());
    assert!(matches!(
        participant.failure.lock().as_ref(),
        Some(ProfileApplicationError::Store(_))
    ));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.candidate.id.as_str())
    );
}

#[tokio::test]
async fn prepared_tun_directory_cannot_bypass_service_admission() {
    let fixture = crate::core_session::test_support::OwnedCoreFixture::new(
        "geodata-directory-service-admission",
    )
    .await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ProfileStore::new(home.join("profiles")).unwrap();
    let controlled = ControlledConfigStore::new(home.join("controlled"));
    fs::create_dir_all(controlled.root()).unwrap();
    fs::write(controlled.runtime_path(), "tun: {enable: false}\n").unwrap();
    let source = home.join("tun-source.yaml");
    fs::write(&source, "tun: {enable: true}\n").unwrap();
    let application = ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source,
            overrides: vec![],
        })
        .await
        .unwrap();
    assert!(prepared.requires_service());
    let pid = fixture.process.snapshot().pid;
    let result = application.apply_prepared(prepared).await;
    assert!(matches!(
        result,
        ProfileApplyOutcome::Rejected {
            cause: ProfileApplicationError::Service(_),
            ..
        }
    ));
    assert!(store.load().unwrap().profiles.is_empty());
    assert_eq!(
        fs::read_to_string(controlled.runtime_path()).unwrap(),
        "tun: {enable: false}\n"
    );
    assert_eq!(session.generation(), 0);
    assert_eq!(fixture.process.snapshot().pid, pid);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_the_waiter_does_not_abandon_admitted_profile_persistence() {
    let fixture = Fixture::new("cancel-persistence");
    fixture
        .controlled
        .materialize_with_overrides_for_core(
            fixture.store.profile_path(&fixture.previous),
            &[],
            CoreKind::Mihomo,
        )
        .unwrap();
    let (address, server) =
        response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
    let mut application = fixture.application(address);
    let session = application.session.clone();
    let (gate, waiting, release) = crate::controlled_config::CommitGate::new();
    application.commit_gate = Some(gate);
    let id = fixture.candidate.id.clone();
    let outer = tokio::spawn(async move {
        application
            .apply(ProfileChange::ActivateExisting {
                id,
                overrides: vec![],
            })
            .await
    });
    waiting.await.unwrap();
    outer.abort();
    assert!(outer.await.unwrap_err().is_cancelled());
    let shutdown_session = session.clone();
    let shutdown = tokio::spawn(async move { shutdown_session.shutdown().await });
    tokio::task::yield_now().await;
    let shutdown_waited = !shutdown.is_finished();
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fixture.store.load().unwrap().active.as_deref()
                == Some(fixture.candidate.id.as_str())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    shutdown.await.unwrap().unwrap();
    server.join().unwrap();
    assert!(
        shutdown_waited,
        "shutdown must wait for the admitted persistence owner"
    );
    assert!(
        std::fs::read_to_string(fixture.controlled.runtime_path())
            .unwrap()
            .contains("MATCH,REJECT")
    );
    // Shutdown waits for admitted persistence, then publishes its own transition.
    assert_eq!(session.generation(), 2);
    assert_eq!(
        session.committed_profile_snapshot().profile_path.as_deref(),
        Some(fixture.store.profile_path(&fixture.candidate).as_path())
    );
}

#[tokio::test]
async fn prepared_local_directory_applies_final_layers_after_override_is_deleted() {
    let fixture = crate::core_session::test_support::OwnedCoreFixture::new(
        "geodata-prepared-directory-final-layers",
    )
    .await;
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ProfileStore::new(home.join("profiles")).unwrap();
    let controlled = ControlledConfigStore::new(home.join("controlled"));
    let source = home.join("new-source.yaml");
    fs::write(&source, "tun: {enable: true}\nmode: global\n").unwrap();
    let override_path = home.join("ordered.yaml");
    fs::write(&override_path, "tun: {enable: false}\nmode: direct\n").unwrap();
    let application = ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source: source.clone(),
            overrides: vec![override_path.clone()],
        })
        .await
        .unwrap();
    assert!(!prepared.requires_service(), "final override disables TUN");
    fs::remove_file(&source).unwrap();
    fs::remove_file(override_path).unwrap();
    let outcome = application.apply_prepared(prepared).await;
    assert!(
        matches!(
            outcome,
            ProfileApplyOutcome::Applied {
                runtime_version: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    let yaml: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(controlled.runtime_path()).unwrap()).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(yaml["mode"].as_str(), Some("direct"));
    let catalog = store.load().unwrap();
    assert_eq!(catalog.profiles.len(), 1);
    assert_eq!(
        fs::read_to_string(store.profile_path(&catalog.profiles[0])).unwrap(),
        "tun: {enable: true}\nmode: global\n"
    );
    session.shutdown().await.unwrap();
    assert!(fixture.process.snapshot().pid.is_none());
}

#[tokio::test]
async fn prepared_local_directory_rejects_changed_controlled_layer_without_committing() {
    let fixture = crate::core_session::test_support::OwnedCoreFixture::new(
        "geodata-prepared-directory-controlled-conflict",
    )
    .await;
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = ProfileStore::new(home.join("profiles")).unwrap();
    let controlled = ControlledConfigStore::new(home.join("controlled"));
    let source = home.join("new-source.yaml");
    fs::write(&source, "tun: {enable: false}\nmode: rule\n").unwrap();
    let application = ProfileApplication::new(store.clone(), controlled.clone(), session.clone());
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source,
            overrides: vec![],
        })
        .await
        .unwrap();
    fs::create_dir_all(controlled.root()).unwrap();
    let changed = "tun: {enable: true}\nmode: global\n";
    fs::write(controlled.root().join("override.yaml"), changed).unwrap();
    let pid = fixture.process.snapshot().pid;
    let outcome = application.apply_prepared(prepared).await;
    assert!(
        matches!(outcome, ProfileApplyOutcome::Rejected { .. }),
        "{outcome:?}"
    );
    assert_eq!(session.generation(), 0);
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert!(store.load().unwrap().profiles.is_empty());
    assert!(!controlled.runtime_path().exists());
    assert_eq!(
        fs::read_to_string(controlled.root().join("override.yaml")).unwrap(),
        changed
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn prepared_import_releases_restore_authority_and_applies_held_source() {
    let fixture = Fixture::new("prepared-import-held");
    let source = fixture.write_source("held.yaml", "HELD");
    let (address, server) = response_server(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let application = fixture.application(address);
    let prepared = application
        .prepare_change(ProfileChange::ImportLocal {
            source: source.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    assert_eq!(fixture.store.load().unwrap().profiles.len(), 2);
    assert_eq!(
        fs::read_dir(fixture.store.root().join("staging"))
            .unwrap()
            .count(),
        0
    );
    let root = fixture.root.clone();
    let exclusive = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::task::spawn_blocking(move || {
            crate::data_coordinator::DataWriteLease::exclusive([root])
        }),
    )
    .await
    .expect("preparation must release restore admission")
    .unwrap();
    drop(exclusive);
    fs::remove_file(&source).unwrap();
    let outcome = application.apply_prepared(prepared).await;
    let request = server.join().unwrap();
    let ProfileApplyOutcome::Applied {
        path,
        runtime_version,
        ..
    } = outcome
    else {
        panic!("held import failed: {outcome:?}");
    };
    assert_eq!(runtime_version, 1);
    assert!(request.starts_with("PUT /configs?force=true "));
    assert_eq!(fs::read_to_string(path).unwrap(), profile_payload("HELD"));
    assert!(!source.exists());
    assert_eq!(
        fs::read_dir(fixture.store.root().join("staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn matching_source_metadata_cannot_authorize_different_source_bytes() {
    let fixture = Fixture::new("same-metadata-different-bytes");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let application = fixture.application(listener.local_addr().unwrap());
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let before = ProfileSourceMetadata::from(prepared.source_candidate.record());
    let path = fixture.store.profile_path(&fixture.candidate);
    let original = fs::read(&path).unwrap();
    let replacement = profile_payload("DIRECT");
    assert_eq!(original.len(), replacement.len());
    fs::write(&path, &replacement).unwrap();
    let current = fixture
        .store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|record| record.id == fixture.candidate.id)
        .unwrap();
    assert_eq!(ProfileSourceMetadata::from(&current), before);
    let outcome = application.apply_prepared(prepared).await;
    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(application.session.generation(), 0);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(fs::read_to_string(path).unwrap(), replacement);
}

#[tokio::test]
async fn prepared_activation_rejects_changed_source_before_runtime_apply() {
    let fixture = Fixture::new("prepared-activation-changed");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let application = fixture.application(listener.local_addr().unwrap());
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let path = fixture.store.profile_path(&fixture.candidate);
    fs::write(&path, profile_payload("CHANGED")).unwrap();
    let outcome = application.apply_prepared(prepared).await;
    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(application.session.generation(), 0);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        profile_payload("CHANGED")
    );
}

#[tokio::test]
async fn prepared_activation_cannot_cross_sessions_with_matching_versions() {
    let fixture = Fixture::new("prepared-cross-session");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let application = fixture.application(address);
    let other = fixture.application(address);
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    assert_eq!(application.session.generation(), other.session.generation());
    let outcome = other.apply_prepared(prepared).await;
    assert!(matches!(
        outcome,
        ProfileApplyOutcome::Rejected {
            cause: ProfileApplicationError::Controller(MihomoError::StaleBinding),
            ..
        }
    ));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn prepared_activation_cannot_replace_a_newer_runtime_transition() {
    let fixture = Fixture::new("prepared-stale-generation");
    let (address, server) = response_server(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let application = fixture.application(address);
    let prepared = application
        .prepare_change(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![],
        })
        .await
        .unwrap();
    let accepted = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.previous.id.clone(),
            overrides: vec![],
        })
        .await;
    assert!(matches!(
        accepted,
        ProfileApplyOutcome::Applied {
            runtime_version: 1,
            ..
        }
    ));
    server.join().unwrap();
    let rejected = application.apply_prepared(prepared).await;
    assert!(matches!(
        rejected,
        ProfileApplyOutcome::Rejected {
            cause: ProfileApplicationError::Controller(MihomoError::StaleBinding),
            ..
        }
    ));
    assert_eq!(application.session.generation(), 1);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn staged_profile_tampering_rolls_back_the_held_candidate_without_committing_catalog() {
    let fixture = Fixture::new("held-source_candidate-source");
    fixture
        .controlled
        .materialize(fixture.store.profile_path(&fixture.previous))
        .unwrap();
    let previous = fixture
        .controlled
        .cached_runtime_payload()
        .unwrap()
        .unwrap();
    let payload = format!("tun: {{enable: false}}\n{}", profile_payload("HELD"));
    let source_candidate = ProfileSourceCandidate::new(
        fixture.store.clone(),
        "Held".into(),
        ProfileSource::Local {
            original_path: fixture.root.join("original.yaml").display().to_string(),
        },
        payload,
        SubscriptionMetadata::default(),
    )
    .unwrap();
    fs::write(
        source_candidate
            .staging_path()
            .expect("materialized source candidate"),
        format!("tun: {{enable: true}}\n{}", profile_payload("CHANGED")),
    )
    .unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut requests = Vec::new();
        while requests.len() < 2 && std::time::Instant::now() < deadline {
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let request =
                crate::test_support::read_controller_request_blocking(&mut reader).unwrap();
            let bytes = request.body();
            requests.push(serde_json::from_slice::<serde_json::Value>(bytes).unwrap());
            reader
                .get_mut()
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        }
        requests
    });
    let application = fixture.application(address);
    let outcome = application
        .apply_source_candidate(source_candidate, vec![])
        .await;
    let requests = server.join().unwrap();
    assert!(matches!(
        outcome,
        ProfileApplyOutcome::RolledBack {
            runtime_version: 1,
            ..
        }
    ));
    let accepted: serde_yaml::Value =
        serde_yaml::from_str(requests[0]["payload"].as_str().unwrap()).unwrap();
    assert_eq!(accepted["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(accepted["rules"][0].as_str(), Some("MATCH,HELD"));
    assert_eq!(requests[1]["payload"].as_str(), Some(previous.as_str()));
    assert_eq!(
        fixture
            .controlled
            .cached_runtime_payload()
            .unwrap()
            .as_deref(),
        Some(previous.as_str())
    );
    let catalog = fixture.store.load().unwrap();
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(catalog.profiles.len(), 2);
    assert_eq!(
        fs::read_dir(fixture.store.root().join("staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn existing_profile_is_applied_through_one_transaction_interface() {
    let fixture = Fixture::new("applied");
    let (address, server) =
        response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    let request = server.join().unwrap();

    let ProfileApplyOutcome::Applied {
        profile,
        source_version,
        runtime_version,
        kind,
        ..
    } = outcome
    else {
        panic!("expected applied profile outcome");
    };
    assert!(request.starts_with("PUT /configs?force=true "));
    assert_eq!(profile.id, fixture.candidate.id);
    assert_eq!(source_version.profile_id, profile.id);
    assert_eq!(runtime_version, 1);
    assert_eq!(kind, CoreApplyKind::HotReloaded);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(profile.id.as_str())
    );
}

#[tokio::test]
async fn active_profile_is_not_committed_before_runtime_accepts_the_candidate() {
    let fixture = Fixture::new("source_candidate-activation");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let observed_store = fixture.store.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut first_byte = [0_u8; 1];
        stream.read_exact(&mut first_byte).unwrap();
        let active = observed_store.load().unwrap().active;

        let mut reader = std::io::Read::chain(std::io::Cursor::new(first_byte), &mut stream);
        let request = crate::test_support::read_controller_request_blocking(&mut reader).unwrap();
        let _body = request.body();

        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        active
    });
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    let active_during_runtime_apply = server.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Applied { .. }));
    assert_eq!(active_during_runtime_apply, Some(fixture.previous.id));
}

#[tokio::test]
async fn runtime_is_restored_when_the_staged_source_changes_before_commit() {
    let fixture = Fixture::new("commit-race");
    fixture
        .controlled
        .materialize_with_overrides_for_core(
            fixture.store.profile_path(&fixture.previous),
            &[],
            CoreKind::Mihomo,
        )
        .unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let candidate_path = fixture.store.profile_path(&fixture.candidate);
    let server = thread::spawn(move || {
        let mut observed = Vec::new();
        for request_index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            observed.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            if request_index == 0 {
                fs::write(&candidate_path, profile_payload("RACE")).unwrap();
            }
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
        observed
    });
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    let observed_runtime_payloads = server.join().unwrap();

    assert!(matches!(
        outcome,
        ProfileApplyOutcome::RolledBack {
            runtime_version: 1,
            ..
        }
    ));
    assert!(observed_runtime_payloads[0].contains("MATCH,REJECT"));
    assert!(observed_runtime_payloads[1].contains("MATCH,DIRECT"));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn runtime_rejection_leaves_the_previous_active_profile_untouched() {
    let fixture = Fixture::new("rolled-back");
    let (address, server) = response_server(api_error_response("rejected"));
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    server.join().unwrap();

    let ProfileApplyOutcome::Rejected {
        last_known_good,
        cause,
    } = outcome
    else {
        panic!("expected rejected profile outcome");
    };
    assert_eq!(
        last_known_good.map(|version| version.profile_id),
        Some(fixture.previous.id.clone())
    );
    assert!(matches!(cause, ProfileApplicationError::Runtime(_)));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn interrupted_runtime_response_is_reported_as_unknown_without_source_commit() {
    let fixture = Fixture::new("runtime-unknown");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8_192];
        let bytes = stream.read(&mut request).unwrap();
        assert!(bytes > 0);
    });
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    server.join().unwrap();

    let ProfileApplyOutcome::RuntimeUnknown {
        recovery,
        cause,
        runtime_version,
    } = outcome
    else {
        panic!("expected runtime-unknown outcome");
    };
    assert_eq!(runtime_version, 1);
    assert!(matches!(cause, ProfileApplicationError::Runtime(_)));
    assert_eq!(
        recovery.last_known_good.map(|version| version.profile_id),
        Some(fixture.previous.id.clone())
    );
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(
        fs::read_dir(fixture.store.root().join("staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn missing_profile_is_rejected_without_contacting_the_runtime() {
    let fixture = Fixture::new("missing");
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let application =
        ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: "missing".into(),
            overrides: Vec::new(),
        })
        .await;

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn local_import_uses_the_same_apply_transaction() {
    let fixture = Fixture::new("import-applied");
    let source = fixture.write_source("imported.yaml", "DIRECT");
    let (address, server) =
        response_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into());
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ImportLocal {
            source,
            overrides: Vec::new(),
        })
        .await;
    server.join().unwrap();

    let ProfileApplyOutcome::Applied { profile, .. } = outcome else {
        panic!("expected applied imported profile");
    };
    let catalog = fixture.store.load().unwrap();
    assert_eq!(catalog.active.as_deref(), Some(profile.id.as_str()));
    assert!(catalog.profiles.iter().any(|item| item.id == profile.id));
}

#[tokio::test]
async fn rejected_local_import_never_commits_the_new_source() {
    let fixture = Fixture::new("import-rejected");
    let source = fixture.write_source("rejected-import.yaml", "REJECT");
    let profiles_before = fixture.store.load().unwrap().profiles.len();
    let (address, server) = response_server(api_error_response("rejected"));
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::ImportLocal {
            source,
            overrides: Vec::new(),
        })
        .await;
    server.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    let catalog = fixture.store.load().unwrap();
    assert_eq!(catalog.profiles.len(), profiles_before);
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn downloaded_source_passes_through_runtime_apply_without_early_persistence() {
    let fixture = Fixture::new("remote-rejected");
    let profiles_before = fixture.store.load().unwrap().profiles.len();
    let (origin_address, origin) =
        response_server(http_ok_response("text/yaml", &profile_payload("REJECT")));
    let (controller_address, controller) = response_server(api_error_response("rejected"));
    let application = fixture.application(controller_address);

    let outcome = application
        .apply(ProfileChange::AddRemote {
            name: String::new(),
            url: format!("http://{origin_address}/profile.yaml"),
            user_agent: "ZenClash-ProfileApplication-Test".into(),
            options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
            overrides: Vec::new(),
        })
        .await;
    origin.join().unwrap();
    controller.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    let catalog = fixture.store.load().unwrap();
    assert_eq!(catalog.profiles.len(), profiles_before);
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn fallback_download_still_requires_final_runtime_acceptance() {
    let fixture = Fixture::new("fallback-download-rejected");
    let profiles_before = fixture.store.load().unwrap().profiles.len();
    let unavailable = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let unavailable_address = unavailable.local_addr().unwrap();
    let proxy = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let proxy_server = thread::spawn(move || {
        let (mut stream, _) = proxy.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        let payload = profile_payload("FALLBACK");
        stream
            .write_all(http_ok_response("text/yaml", &payload).as_bytes())
            .unwrap();
        String::from_utf8_lossy(&request[..bytes]).into_owned()
    });
    let controller = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let controller_address = controller.local_addr().unwrap();
    let controller_server = thread::spawn(move || {
        let mut requests = Vec::new();
        for request_index in 0..2 {
            let (mut stream, _) = controller.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            let response = if request_index == 0 {
                let body = format!(r#"{{"mixed-port":{}}}"#, proxy_address.port());
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                api_error_response("rejected after fallback")
            };
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let application = fixture.application(controller_address);
    let subscription_url = format!("http://{unavailable_address}/profile.yaml");

    let outcome = application
        .apply(ProfileChange::AddRemote {
            name: "Fallback Candidate".into(),
            url: subscription_url.clone(),
            user_agent: "ZenClash-Fallback-Application-Test".into(),
            options: RemoteProfileOptions::default()
                .with_download_policy(1, false)
                .unwrap(),
            overrides: Vec::new(),
        })
        .await;
    drop(unavailable);
    let proxy_request = proxy_server.join().unwrap();
    let controller_requests = controller_server.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert!(proxy_request.starts_with(&format!("GET {subscription_url} HTTP/1.1")));
    assert!(controller_requests[0].starts_with("GET /configs "));
    assert!(controller_requests[1].starts_with("PUT /configs?force=true "));
    let catalog = fixture.store.load().unwrap();
    assert_eq!(catalog.profiles.len(), profiles_before);
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn rejected_active_yaml_edit_preserves_the_source_and_active_revision() {
    let fixture = Fixture::new("edit-rejected");
    let path = fixture.store.profile_path(&fixture.previous);
    let original = fs::read_to_string(&path).unwrap();
    let (address, server) = response_server(api_error_response("rejected"));
    let application = fixture.application(address);

    let outcome = application
        .apply(ProfileChange::EditYaml {
            id: fixture.previous.id.clone(),
            expected_payload: original.clone(),
            new_payload: profile_payload("REJECT"),
            overrides: Vec::new(),
        })
        .await;
    server.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn inactive_remote_update_is_validated_and_stored_without_runtime_apply() {
    let fixture = Fixture::new("inactive-remote-update");
    let (origin_address, origin) =
        response_server(http_ok_response("text/yaml", &profile_payload("UPDATED")));
    let remote = fixture
        .store
        .store_profile(
            "Remote".into(),
            ProfileSource::Remote {
                url: format!("http://{origin_address}/profile.yaml"),
                user_agent: "ZenClash-Test".into(),
                options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
            },
            &profile_payload("OLD"),
        )
        .unwrap();
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let application = ProfileApplication::new(
        fixture.store.clone(),
        fixture.controlled.clone(),
        session.clone(),
    );

    let outcome = application
        .apply(ProfileChange::UpdateRemote {
            id: remote.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    origin.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Stored { .. }));
    assert_eq!(session.snapshot().generation, 0);
    assert!(
        fs::read_to_string(fixture.store.profile_path(&remote))
            .unwrap()
            .contains("MATCH,UPDATED")
    );
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
}

#[tokio::test]
async fn staged_download_metadata_fixed_interval_preserves_other_subscription_facts() {
    let fixture = Fixture::new("staged-metadata-fixed");
    let payload = profile_payload("METADATA-UPDATED");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nProfile-Web-Page-Url: https://example.com/new-account\r\nProfile-Update-Interval: 2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let (address, server) = response_server(response);
    let url = format!("http://{address}/profile.yaml");
    let options = RemoteProfileOptions::new("Bearer staged-metadata-secret", false)
        .unwrap()
        .with_route(RemoteProfileRoute::Direct)
        .with_download_policy(45, true)
        .unwrap();
    let remote = fixture
        .store
        .store_profile_with_subscription(
            "Remote".into(),
            ProfileSource::Remote {
                url: url.clone(),
                user_agent: "ZenClash-Test".into(),
                options: options.clone(),
            },
            &profile_payload("OLD"),
            SubscriptionMetadata {
                usage: Some(crate::SubscriptionUsage {
                    upload: 10,
                    download: 20,
                    total: 1000,
                    expire: 0,
                }),
                home_url: Some("https://example.com/old-account".into()),
                suggested_update_interval_minutes: Some(360),
            },
        )
        .unwrap();
    fixture
        .store
        .set_update_policy(&remote.id, true, 45)
        .unwrap();
    fixture
        .store
        .set_remote_request_settings(
            &remote.id,
            "Remote",
            &url,
            "ZenClash-Test",
            options,
            Some("0 */3 * * *".into()),
        )
        .unwrap();
    let expected = fixture
        .store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|p| p.id == remote.id)
        .unwrap();
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let application = ProfileApplication::new(
        fixture.store.clone(),
        fixture.controlled.clone(),
        session.clone(),
    );

    let outcome = application
        .apply(ProfileChange::UpdateRemote {
            id: remote.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    let request = server.join().unwrap();
    assert!(
        matches!(outcome, ProfileApplyOutcome::Stored { .. }),
        "{outcome:?}"
    );
    assert!(request.contains("authorization: Bearer staged-metadata-secret\r\n"));
    let catalog = fixture.store.load().unwrap();
    let updated = catalog.profiles.iter().find(|p| p.id == remote.id).unwrap();
    assert_eq!(updated.subscription.usage, expected.subscription.usage);
    assert_eq!(
        updated.subscription.home_url.as_deref(),
        Some("https://example.com/new-account")
    );
    assert_eq!(
        updated.subscription.suggested_update_interval_minutes,
        Some(120)
    );
    assert_eq!(updated.update_interval_minutes, 45);
    assert_eq!(updated.update_cron, expected.update_cron);
    assert!(updated.auto_update);
    assert_eq!(updated.source, expected.source);
    assert_eq!(updated.file_name, expected.file_name);
    assert_eq!(updated.size_bytes, payload.len() as u64);
    assert!(updated.updated_at >= expected.updated_at);
    assert!(updated.updated_at <= crate::profiles::unix_timestamp());
    assert_eq!(
        fs::read_to_string(fixture.store.profile_path(updated)).unwrap(),
        payload
    );
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(session.snapshot().generation, 0);
}

#[tokio::test]
async fn staged_download_metadata_provider_interval_preserves_other_subscription_facts() {
    let fixture = Fixture::new("staged-metadata-provider");
    let payload = profile_payload("METADATA-UPDATED");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nProfile-Web-Page-Url: https://example.com/new-account\r\nProfile-Update-Interval: 2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let (address, server) = response_server(response);
    let url = format!("http://{address}/profile.yaml");
    let options = RemoteProfileOptions::new("Bearer staged-metadata-secret", false)
        .unwrap()
        .with_route(RemoteProfileRoute::Direct)
        .with_download_policy(45, false)
        .unwrap();
    let remote = fixture
        .store
        .store_profile_with_subscription(
            "Remote".into(),
            ProfileSource::Remote {
                url: url.clone(),
                user_agent: "ZenClash-Test".into(),
                options: options.clone(),
            },
            &profile_payload("OLD"),
            SubscriptionMetadata {
                usage: Some(crate::SubscriptionUsage {
                    upload: 10,
                    download: 20,
                    total: 1000,
                    expire: 0,
                }),
                home_url: Some("https://example.com/old-account".into()),
                suggested_update_interval_minutes: Some(360),
            },
        )
        .unwrap();
    fixture
        .store
        .set_update_policy(&remote.id, true, 45)
        .unwrap();
    fixture
        .store
        .set_remote_request_settings(
            &remote.id,
            "Remote",
            &url,
            "ZenClash-Test",
            options,
            Some("0 */3 * * *".into()),
        )
        .unwrap();
    let expected = fixture
        .store
        .load()
        .unwrap()
        .profiles
        .into_iter()
        .find(|p| p.id == remote.id)
        .unwrap();
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let application = ProfileApplication::new(
        fixture.store.clone(),
        fixture.controlled.clone(),
        session.clone(),
    );

    let outcome = application
        .apply(ProfileChange::UpdateRemote {
            id: remote.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    let request = server.join().unwrap();
    assert!(
        matches!(outcome, ProfileApplyOutcome::Stored { .. }),
        "{outcome:?}"
    );
    assert!(request.contains("authorization: Bearer staged-metadata-secret\r\n"));
    let catalog = fixture.store.load().unwrap();
    let updated = catalog.profiles.iter().find(|p| p.id == remote.id).unwrap();
    assert_eq!(updated.subscription.usage, expected.subscription.usage);
    assert_eq!(
        updated.subscription.home_url.as_deref(),
        Some("https://example.com/new-account")
    );
    assert_eq!(
        updated.subscription.suggested_update_interval_minutes,
        Some(120)
    );
    assert_eq!(updated.update_interval_minutes, 120);
    assert_eq!(updated.update_cron, expected.update_cron);
    assert!(updated.auto_update);
    assert_eq!(updated.source, expected.source);
    assert_eq!(updated.file_name, expected.file_name);
    assert_eq!(updated.size_bytes, payload.len() as u64);
    assert!(updated.updated_at >= expected.updated_at);
    assert!(updated.updated_at <= crate::profiles::unix_timestamp());
    assert_eq!(
        fs::read_to_string(fixture.store.profile_path(updated)).unwrap(),
        payload
    );
    assert_eq!(
        catalog.active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(session.snapshot().generation, 0);
}

#[tokio::test]
async fn rejected_active_remote_update_preserves_the_downloaded_source_lkg() {
    let fixture = Fixture::new("active-remote-update-rejected");
    let (origin_address, origin) =
        response_server(http_ok_response("text/yaml", &profile_payload("NEW")));
    let remote = fixture
        .store
        .store_profile(
            "Remote".into(),
            ProfileSource::Remote {
                url: format!("http://{origin_address}/profile.yaml"),
                user_agent: "ZenClash-Test".into(),
                options: RemoteProfileOptions::default().with_route(RemoteProfileRoute::Direct),
            },
            &profile_payload("OLD"),
        )
        .unwrap();
    fixture.store.activate(&remote.id).unwrap();
    let path = fixture.store.profile_path(&remote);
    let original = fs::read_to_string(&path).unwrap();
    let (controller_address, controller) = response_server(api_error_response("rejected"));
    let application = fixture.application(controller_address);

    let outcome = application
        .apply(ProfileChange::UpdateRemote {
            id: remote.id.clone(),
            overrides: Vec::new(),
        })
        .await;
    origin.join().unwrap();
    controller.join().unwrap();

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(remote.id.as_str())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn final_merged_candidate_is_validated_before_runtime_or_persistence() {
    use std::os::unix::fs::PermissionsExt as _;

    use crate::CoreConfigValidator;

    let fixture = Fixture::new("merged-validation-rejected");
    let override_path = fixture.write_source("rejecting-override.yaml", "REJECT_FINAL");
    let validator = fixture.root.join("reject-final.sh");
    fs::write(
        &validator,
        "#!/bin/sh\nfile=''\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = '-f' ]; then shift; file=\"$1\"; fi\n  shift\ndone\nif grep -q 'REJECT_FINAL' \"$file\"; then exit 1; fi\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&validator).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&validator, permissions).unwrap();
    let client = MihomoClient::new(MihomoEndpoint::default())
        .unwrap()
        .with_config_validator(CoreConfigValidator::new(
            CoreKind::Mihomo,
            validator,
            fixture.root.join("validator-home"),
        ))
        .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let application =
        ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![override_path],
        })
        .await;

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    let staging = fixture.store.root().join("staging");
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
    assert_eq!(application.session.snapshot().generation, 0);
}

#[tokio::test]
async fn override_parse_failure_only_removes_the_staging_candidate() {
    let fixture = Fixture::new("override-parse-rejected");
    let override_path = fixture.root.join("sources").join("invalid-override.yaml");
    fs::write(&override_path, "rules: [").unwrap();
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let application =
        ProfileApplication::new(fixture.store.clone(), fixture.controlled.clone(), session);

    let outcome = application
        .apply(ProfileChange::ActivateExisting {
            id: fixture.candidate.id.clone(),
            overrides: vec![override_path],
        })
        .await;

    assert!(matches!(outcome, ProfileApplyOutcome::Rejected { .. }));
    assert_eq!(
        fixture.store.load().unwrap().active.as_deref(),
        Some(fixture.previous.id.as_str())
    );
    assert_eq!(
        fs::read_dir(fixture.store.root().join("staging"))
            .unwrap()
            .count(),
        0
    );
    assert!(!fixture.controlled.runtime_path().exists());
}

struct Fixture {
    root: PathBuf,
    store: ProfileStore,
    controlled: ControlledConfigStore,
    previous: ProfileRecord,
    candidate: ProfileRecord,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-profile-application-{name}-{}",
            std::process::id()
        ));
        let source_root = root.join("sources");
        fs::create_dir_all(&source_root).unwrap();
        let previous_source = source_root.join("previous.yaml");
        let candidate_source = source_root.join("candidate.yaml");
        fs::write(&previous_source, profile_payload("DIRECT")).unwrap();
        fs::write(&candidate_source, profile_payload("REJECT")).unwrap();
        let store = ProfileStore::new(root.join("profiles")).unwrap();
        let previous = store.import_local(previous_source).unwrap();
        let candidate = store.import_local(candidate_source).unwrap();
        store.activate(&previous.id).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        Self {
            root,
            store,
            controlled,
            previous,
            candidate,
        }
    }

    fn application(&self, address: std::net::SocketAddr) -> ProfileApplication {
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        ProfileApplication::new(self.store.clone(), self.controlled.clone(), session)
    }

    fn write_source(&self, name: &str, target: &str) -> PathBuf {
        let path = self.root.join("sources").join(name);
        fs::write(&path, profile_payload(target)).unwrap();
        path
    }
}

fn profile_payload(target: &str) -> String {
    format!("mode: rule\nproxies: []\nproxy-groups: []\nrules:\n  - MATCH,{target}\n")
}

fn response_server(response: String) -> (std::net::SocketAddr, thread::JoinHandle<String>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&request[..bytes]).into_owned()
    });
    (address, server)
}

fn api_error_response(message: &str) -> String {
    let body = format!(r#"{{"message":"{message}"}}"#);
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn http_ok_response(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
