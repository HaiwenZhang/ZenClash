use super::*;
use super::{profile_application::ChangeContext, recovery_record::ServiceRuntimeFacts};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use zenclash_core::{
    CoreApplyOutcome, ProfileApplication, ProfileApplyOutcome, ProfileChange, ProfileStore,
};

#[tokio::test]
async fn service_config_reapply_without_a_committed_profile_preserves_the_noop_result() {
    let (commands, store) = maintenance_fixture();
    assert!(
        commands
            .reapply_with_overrides(store.clone(), vec![])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(commands.session.generation(), 0);
    assert!(!store.root().exists());
    assert!(commands.service_state().unwrap().health().is_none());
    assert!(commands.pending_finalization().is_none());
    commands.session.shutdown().await.unwrap();
}

fn maintenance_fixture() -> (ApplicationCommands, ControlledConfigStore) {
    let home = std::env::temp_dir().join(format!(
        "zenclash-ui-maintenance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ControlledConfigStore::new(home.join("controlled"));
    let process =
        zenclash_core::ManagedCoreProcess::prepare_stopped(zenclash_core::CoreLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: home.join("ordinary-mihomo"),
            config_file: home.join("missing-source.yaml"),
            home_dir: home,
            endpoint: zenclash_core::MihomoEndpoint::default(),
            controller_override: None,
        });
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process).unwrap(),
    )
    .unwrap();
    let capture =
        zenclash_core::TrafficCaptureSession::new(session.clone(), store.clone(), None, None);
    let manager = ServiceManager::new(session.clone(), capture);
    (
        ApplicationCommands::new(session, None).with_service_manager(manager),
        store,
    )
}

#[tokio::test]
async fn maintenance_preparation_survives_page_owner_drop_and_consent_rejection() {
    let (page, store) = maintenance_fixture();
    let shared = page.clone();
    let request = page
        .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
        .unwrap();
    let preparation = page
        .prepare_service_maintenance(request, &store)
        .await
        .unwrap();
    drop(page);
    assert!(Arc::ptr_eq(
        &shared.service_maintenance_preparation().unwrap(),
        &preparation
    ));
    assert!(
        shared
            .maintain_prepared_service(preparation.clone(), false)
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &shared.service_maintenance_preparation().unwrap(),
        &preparation
    ));
    assert!(preparation.ready());
    assert!(shared.service_local_recovery().is_none());
    assert!(
        !store.root().exists(),
        "Local preparation performed filesystem writes"
    );
}

#[tokio::test]
async fn replaced_maintenance_preparation_cannot_submit_native_work() {
    let (commands, store) = maintenance_fixture();
    let request = commands
        .request_service_maintenance(zenclash_core::ServiceOperation::Repair, None)
        .unwrap();
    let previous = commands
        .prepare_service_maintenance(request, &store)
        .await
        .unwrap();
    let request = commands
        .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
        .unwrap();
    let current = commands
        .prepare_service_maintenance(request, &store)
        .await
        .unwrap();
    assert!(
        commands
            .maintain_prepared_service(previous, true)
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &commands.service_maintenance_preparation().unwrap(),
        &current
    ));
    assert_eq!(commands.service_state().unwrap().revision(), 2);
    assert!(!store.root().exists());
}

#[tokio::test]
async fn maintenance_business_command_rejects_overlap_before_preparation() {
    let (commands, store) = maintenance_fixture();
    let shared = commands.clone();
    let _command = commands.maintenance_command.lock().await;
    let request = shared
        .request_service_maintenance(zenclash_core::ServiceOperation::Repair, None)
        .unwrap();
    assert!(
        shared
            .prepare_service_maintenance(request, &store)
            .await
            .is_err()
    );
    assert!(shared.service_maintenance_preparation().is_none());
    assert_eq!(shared.service_state().unwrap().revision(), 0);
}

#[tokio::test]
async fn cancelled_maintenance_waiter_still_publishes_shared_preparation() {
    use std::{future::Future, task::Poll};

    let (commands, store) = maintenance_fixture();
    let request = commands
        .request_service_maintenance(zenclash_core::ServiceOperation::Uninstall, None)
        .unwrap();
    let mut waiter = Box::pin(commands.prepare_service_maintenance(request, &store));
    std::future::poll_fn(|cx| {
        assert!(waiter.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(waiter);
    let _finished = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        commands.maintenance_command.lock(),
    )
    .await
    .unwrap();
    let preparation = commands.service_maintenance_preparation().unwrap();
    assert!(preparation.ready());
    assert_eq!(
        preparation.operation(),
        zenclash_core::ServiceOperation::Uninstall
    );
    assert!(!store.root().exists());
}

fn committed_fixture() -> (
    PathBuf,
    ProfileStore,
    ApplicationCommands,
    Arc<ProfileApplyOutcome>,
) {
    let root = std::env::temp_dir().join(format!(
        "zenclash-committed-confirmation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ProfileStore::new(root.join("profiles")).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
    let previous = store.import_local(&source).unwrap();
    store.activate(&previous.id).unwrap();
    std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
    let profile = store.import_local(&source).unwrap();
    let path = store.activate(&profile.id).unwrap();
    let client =
        MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path.clone()), Vec::new())
            .unwrap();
    let outcome = Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
        source_version: (&profile).into(),
        profile,
        path,
        cause: zenclash_core::ProfileApplicationError::Task("commit reply lost".into()),
        runtime_version: session.generation(),
    });
    (
        root,
        store,
        ApplicationCommands::new(session, None),
        outcome,
    )
}

#[test]
fn directory_service_facts_keep_same_generation_warning_and_durable_receipt() {
    let (root, _, service, outcome) = committed_fixture();
    let generation = service.session.generation();
    let facts = ServiceRuntimeFacts::new(
        &CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::HotReloaded,
            generation,
        },
        true,
        Some("capture readback failed"),
    );
    let receipt = service
        .finish_apply(outcome, ChangeContext::Selection, Some(facts))
        .unwrap();
    assert_eq!(
        service.service_tun_warning().as_deref(),
        Some("capture readback failed")
    );
    assert_eq!(service.pending_finalization(), Some(generation));
    assert!(receipt.warning().is_some());
    assert!(
        service.latest_recovery().is_none(),
        "durable source must not offer source rollback"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn confirmed_capture_facts_do_not_discard_a_shared_unconfirmed_source_receipt() {
    let (root, _, service, outcome) = committed_fixture();
    let shared = service.clone();
    let original_outcome = Arc::downgrade(&outcome);
    let generation = service.session.generation();
    let facts = ServiceRuntimeFacts::new(
        &CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::HotReloaded,
            generation,
        },
        false,
        Some("independent capture warning"),
    );
    let receipt = service
        .finish_apply(outcome, ChangeContext::Selection, Some(facts))
        .unwrap();
    drop(receipt);
    drop(service);
    assert_eq!(shared.pending_finalization(), Some(generation));
    assert!(
        matches!(
            original_outcome.upgrade().as_deref(),
            Some(ProfileApplyOutcome::CommittedButRuntimeUnknown { .. })
        ),
        "the shared record must retain the original durable source receipt after its foreground owner leaves"
    );
    assert_eq!(
        shared.service_tun_warning().as_deref(),
        Some("independent capture warning")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_service_facts_cannot_overwrite_a_newer_runtime_result() {
    let (root, _, service, outcome) = committed_fixture();
    let generation = service.session.generation();
    let current = CoreApplyOutcome {
        kind: zenclash_core::CoreApplyKind::HotReloaded,
        generation: generation + 1,
    };
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(&current, true, Some("new warning")),
        current.generation,
    );
    let stale = CoreApplyOutcome {
        kind: current.kind,
        generation,
    };
    service
        .finish_apply(
            outcome,
            ChangeContext::Selection,
            Some(ServiceRuntimeFacts::new(&stale, true, Some("old warning"))),
        )
        .unwrap();
    assert_eq!(
        service.service_tun_warning().as_deref(),
        Some("new warning")
    );
    assert_eq!(service.pending_finalization(), Some(current.generation));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stale_profile_completion_cannot_attach_another_runtime_transaction_facts() {
    let (root, _, service, outcome) = committed_fixture();
    service
        .session
        .switch_to_direct(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:2", ""))
        .await
        .unwrap();
    let current = CoreApplyOutcome {
        kind: zenclash_core::CoreApplyKind::HotReloaded,
        generation: service.session.generation(),
    };
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(&current, true, Some("current capture warning")),
        current.generation,
    );
    service
        .finish_apply(
            outcome,
            ChangeContext::Selection,
            Some(ServiceRuntimeFacts::new(
                &current,
                false,
                Some("unrelated transaction warning"),
            )),
        )
        .unwrap();
    assert_eq!(service.pending_finalization(), Some(current.generation));
    assert_eq!(
        service.service_tun_warning().as_deref(),
        Some("current capture warning")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn committed_source_returns_a_saved_receipt_and_separate_confirmation() {
    let (root, store, service, outcome) = committed_fixture();
    let receipt = service
        .finish_apply(outcome, ChangeContext::Selection, None)
        .unwrap();
    assert_eq!(
        receipt.runtime_version(),
        Some(service.session().generation())
    );
    assert!(receipt.warning().is_some());
    assert_eq!(service.pending_finalization(), receipt.runtime_version());
    assert!(
        service.latest_recovery().is_none(),
        "durable data must not offer old-source rollback"
    );
    assert_eq!(
        service.committed_profile().profile_path.as_deref(),
        Some(receipt.path())
    );
    assert_eq!(
        store.active_path().unwrap().as_deref(),
        Some(receipt.path())
    );
    assert!(
        std::fs::read_to_string(receipt.path())
            .unwrap()
            .contains("7891")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_service_confirmation_keeps_the_saved_source_and_pending_action() {
    let (root, store, service, outcome) = committed_fixture();
    let receipt = service
        .finish_apply(outcome, ChangeContext::Editor, None)
        .unwrap();
    let version = receipt.runtime_version().unwrap();
    let before = store.load().unwrap();
    let payload = std::fs::read(receipt.path()).unwrap();
    // A Direct client cannot confirm a Service commit. No activation or reload is allowed.
    assert!(service.confirm_service_runtime(version).await.is_err());
    assert_eq!(service.pending_finalization(), Some(version));
    assert_eq!(store.load().unwrap().active, before.active);
    assert_eq!(std::fs::read(receipt.path()).unwrap(), payload);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn saved_service_tun_uncertainty_keeps_confirmation_without_source_rollback() {
    let (root, store, service, _) = committed_fixture();
    let path = store.active_path().unwrap().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let generation = service.session.generation();
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Restarted,
                generation,
            },
            true,
            None,
        ),
        service.session.generation(),
    );
    assert_eq!(service.pending_finalization(), Some(generation));
    assert!(service.latest_recovery().is_none());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn startup_saved_pending_receipt_is_confirmed_through_the_same_owner() {
    let (root, _, service, _) = committed_fixture();
    let generation = service.session.generation();
    let saved = CoreApplyOutcome {
        kind: zenclash_core::CoreApplyKind::Restarted,
        generation,
    };
    service
        .recovery_record
        .accept_initialization(Some(&saved), true, service.session.generation());
    assert_eq!(service.pending_finalization(), Some(generation));
    // Direct is deliberately not a native Service fixture: confirmation must
    // reject it rather than pretend this UI record can confirm Commit alone.
    assert!(service.confirm_service_runtime(generation).await.is_err());
    assert_eq!(service.pending_finalization(), Some(generation));
    assert!(service.latest_recovery().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_unsaved_or_stale_initialization_cannot_publish_a_receipt() {
    let (root, _, service, _) = committed_fixture();
    service
        .recovery_record
        .accept_initialization(None, true, service.session.generation());
    assert!(service.pending_finalization().is_none());
    let saved = CoreApplyOutcome {
        kind: zenclash_core::CoreApplyKind::Restarted,
        generation: service.session.generation() + 1,
    };
    service
        .recovery_record
        .accept_initialization(Some(&saved), true, service.session.generation());
    assert!(service.pending_finalization().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn saved_capture_readback_failure_is_not_a_commit_to_confirm() {
    let (root, store, service, _) = committed_fixture();
    let path = store.active_path().unwrap().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let generation = service.session.generation();
    let warning = "capture readback failed after successful commit";
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Patched,
                generation,
            },
            false,
            Some(warning),
        ),
        service.session.generation(),
    );
    assert!(service.pending_finalization().is_none());
    assert!(service.confirm_service_runtime(generation).await.is_err());
    assert_eq!(service.service_tun_warning().as_deref(), Some(warning));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_confirmation_does_not_clear_independent_cleanup_warning() {
    let (root, _, service, _) = committed_fixture();
    let generation = service.session.generation();
    let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Patched,
                generation,
            },
            true,
            Some(&warning),
        ),
        service.session.generation(),
    );
    assert!(service.recovery_record.confirm_service_commit(generation));
    assert!(service.pending_finalization().is_none());
    assert_eq!(service.service_tun_warning(), Some(warning));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn late_service_tun_receipt_cannot_clear_a_new_binding_pending_commit() {
    let (root, _, service, outcome) = committed_fixture();
    let old_generation = service.session.generation();
    service
        .session
        .switch_to_direct(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:2", ""))
        .await
        .unwrap();
    let ProfileApplyOutcome::CommittedButRuntimeUnknown {
        profile,
        path,
        source_version,
        ..
    } = outcome.as_ref()
    else {
        unreachable!()
    };
    let current_generation = service.session.generation();
    service.recovery_record.accept_profile_completion(
        &Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile: profile.clone(),
            path: path.clone(),
            source_version: source_version.clone(),
            cause: zenclash_core::ProfileApplicationError::Task("new commit reply lost".into()),
            runtime_version: current_generation,
        }),
        None,
        &service.session,
    );
    service.recovery_record.accept_service_runtime(
        ServiceRuntimeFacts::new(
            &CoreApplyOutcome {
                kind: zenclash_core::CoreApplyKind::Restarted,
                generation: old_generation,
            },
            true,
            None,
        ),
        service.session.generation(),
    );
    assert_eq!(service.pending_finalization(), Some(current_generation));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_successful_mode_change_does_not_make_the_saved_commit_unconfirmable() {
    let (root, _, service, outcome) = committed_fixture();
    let receipt = service
        .finish_apply(outcome, ChangeContext::Selection, None)
        .unwrap();
    let version = receipt.runtime_version().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    service
        .session()
        .switch_to_direct(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        for (method, mode) in [("GET", "rule"), ("PATCH", ""), ("GET", "global")] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            let count = request.len();
            assert!(request[..count].starts_with(format!("{method} /configs").as_bytes()));
            let response = if method == "PATCH" {
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
            } else {
                let body = format!("{{\"mixed-port\":7891,\"mode\":\"{mode}\"}}");
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    service
        .session()
        .set_mode(
            &ControlledConfigStore::new(root.join("controlled")),
            "global",
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert!(service.session().generation() > version);
    assert_eq!(service.pending_finalization(), Some(version));
    let failure = service.confirm_service_runtime(version).await.unwrap_err();
    assert_eq!(
        failure,
        zenclash_i18n::text_with(
            "profiles.recovery.confirm_failed",
            &[(
                "error",
                zenclash_core::MihomoError::StaleBinding.to_string()
            )]
        ),
        "confirmation must reach the client instead of rejecting an unrelated mode generation"
    );
    assert_eq!(service.pending_finalization(), Some(version));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn late_service_confirmation_cannot_clear_a_newer_pending_commit() {
    let (root, _, service, outcome) = committed_fixture();
    service
        .recovery_record
        .accept_profile_completion(&outcome, None, &service.session);
    let ProfileApplyOutcome::CommittedButRuntimeUnknown {
        profile,
        path,
        source_version,
        runtime_version,
        ..
    } = outcome.as_ref()
    else {
        unreachable!()
    };
    service.recovery_record.accept_profile_completion(
        &Arc::new(ProfileApplyOutcome::CommittedButRuntimeUnknown {
            profile: profile.clone(),
            path: path.clone(),
            source_version: source_version.clone(),
            cause: zenclash_core::ProfileApplicationError::Task("second commit reply lost".into()),
            runtime_version: runtime_version + 1,
        }),
        None,
        &service.session,
    );
    assert!(
        !service
            .recovery_record
            .confirm_service_commit(*runtime_version)
    );
    assert_eq!(service.pending_finalization(), Some(runtime_version + 1));
    assert!(
        service
            .recovery_record
            .confirm_service_commit(runtime_version + 1)
    );
    assert!(service.pending_finalization().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn delayed_acceptance_does_not_clear_a_newer_unknown_runtime() {
    Box::pin(assert_delayed_runtime_publication(false, false)).await;
}

#[tokio::test]
async fn delayed_unknown_does_not_replace_a_newer_accepted_runtime() {
    Box::pin(assert_delayed_runtime_publication(true, false)).await;
}

#[tokio::test]
async fn reapplying_the_current_profile_clears_shared_runtime_uncertainty() {
    Box::pin(assert_delayed_runtime_publication(true, true)).await;
}

async fn read_profile_reload(
    listener: &tokio::net::TcpListener,
    deadline: std::time::Duration,
) -> std::io::Result<tokio::net::TcpStream> {
    let (mut stream, _) = tokio::time::timeout(deadline, listener.accept())
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for the profile reload connection",
            )
        })??;
    let request = tokio::time::timeout(
        deadline,
        zenclash_core::test_support::read_controller_request(&mut stream),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out reading the profile reload request",
        )
    })??;
    if !request.headers().starts_with("PUT /configs") {
        return Err(std::io::Error::other(format!(
            "unexpected profile reload request: {}",
            request.headers()
        )));
    }
    Ok(stream)
}

#[tokio::test]
async fn profile_reload_fixture_times_out_when_no_request_arrives() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let error = read_profile_reload(&listener, std::time::Duration::from_millis(25))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("waiting for"));
}

#[tokio::test]
async fn profile_reload_fixture_times_out_when_the_http_body_is_incomplete() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    client
        .write_all(b"PUT /configs HTTP/1.1\r\nContent-Length: 10\r\n\r\n{")
        .await
        .unwrap();
    let error = read_profile_reload(&listener, std::time::Duration::from_millis(25))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("reading"));
    drop(client);
}

async fn assert_delayed_runtime_publication(unknown_first: bool, reapply_current: bool) {
    let root = std::env::temp_dir().join(format!(
        "zenclash-profile-result-order-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ProfileStore::new(root.join("profiles")).unwrap();
    let source = root.join("source.yaml");
    // This fixture tests completion ordering, so proxy-listener availability
    // must not reject a transition before its mocked controller request.
    std::fs::write(&source, "mixed-port: 0\nrules: [MATCH,DIRECT]\n").unwrap();
    let first = store.import_local(&source).unwrap();
    let path = store.activate(&first.id).unwrap();
    std::fs::write(&source, "mixed-port: 0\nrules: [MATCH,REJECT]\n").unwrap();
    let second = store.import_local(&source).unwrap();
    let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
    let controlled = ControlledConfigStore::new(root.join("controlled"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // JoinSet owns cancellation too: a failed outcome assertion must not leave
    // a detached mock server waiting for a request that will never be sent.
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        for lose_response in [unknown_first, !unknown_first] {
            let mut stream = read_profile_reload(&listener, std::time::Duration::from_secs(10))
                .await
                .expect("profile reload fixture failed");
            if !lose_response {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"),
                )
                .await
                .expect("timed out writing the profile reload response")
                .unwrap();
            }
            // Losing the response intentionally leaves runtime acceptance uncertain.
        }
    });
    let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
        format!("http://{address}"),
        "",
    ))
    .unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path), Vec::new()).unwrap();
    let commands = ApplicationCommands::new(session.clone(), Some(overrides));
    // Execute a real first transition, then pause its service continuation at the
    // await boundary while another entry point completes and publishes its result.
    let delayed = Arc::new(
        ProfileApplication::new(store.clone(), controlled.clone(), session)
            .apply(ProfileChange::ActivateExisting {
                id: first.id,
                overrides: Vec::new(),
            })
            .await,
    );
    assert!(
        matches!(
            (unknown_first, delayed.as_ref()),
            (true, ProfileApplyOutcome::RuntimeUnknown { .. })
                | (false, ProfileApplyOutcome::Applied { .. })
        ),
        "unexpected first profile application outcome: {delayed:?}"
    );
    let latest_failed = if reapply_current {
        assert!(
            commands
                .finish_apply(Arc::clone(&delayed), ChangeContext::Selection, None)
                .is_err()
        );
        assert!(commands.latest_recovery().is_some());
        let applied = commands
            .reapply_with_overrides(controlled, Vec::new())
            .await
            .unwrap();
        assert!(applied.is_some());
        assert!(commands.latest_recovery().is_none());
        false
    } else {
        let latest = commands
            .activate(store, controlled, second.id.clone())
            .await;
        if unknown_first {
            latest.expect("the newer profile must be accepted");
            false
        } else {
            let failure = latest.err().expect("the newer runtime must be unknown");
            assert!(
                matches!(
                    failure.retained_outcome.as_deref(),
                    Some(ProfileApplyOutcome::RuntimeUnknown { .. })
                ),
                "unexpected newer profile failure: {failure:?}"
            );
            true
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), server.join_next())
        .await
        .expect("timed out waiting for the profile reload fixture to finish")
        .expect("profile reload fixture was not running")
        .unwrap();
    assert_eq!(latest_failed, !unknown_first);
    let _ = commands.finish_apply(delayed, ChangeContext::Selection, None);
    if unknown_first {
        assert!(
            commands.latest_recovery().is_none(),
            "a delayed unknown result replaced the newer accepted runtime"
        );
    } else {
        assert_eq!(
            commands
                .latest_recovery()
                .map(|recovery| recovery.attempted.profile_id),
            Some(second.id),
            "a delayed accepted result cleared the newer unknown runtime"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn shared_recovery_survives_a_preflight_rejection_until_a_profile_is_accepted() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-profile-service-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ProfileStore::new(root.join("profiles")).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
    let previous = store.import_local(&source).unwrap();
    let path = store.activate(&previous.id).unwrap();
    std::fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
    let candidate = store.import_local(&source).unwrap();
    let overrides = YamlOverrideStore::new(root.join("overrides")).unwrap();
    let controlled = ControlledConfigStore::new(root.join("controlled"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let count = request.len();
        assert!(request[..count].starts_with(b"PUT /configs?force=true "));
        drop(stream); // The request may have been accepted, but its response is lost.
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let count = request.len();
        assert!(request[..count].starts_with(b"PUT /configs?force=true "));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let count = request.len();
        assert!(request[..count].starts_with(b"GET /configs"));
        let body = r#"{"mixed-port":7890,"mode":"rule"}"#;
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let count = request.len();
        assert!(request[..count].starts_with(b"PATCH /configs"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = zenclash_core::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        let count = request.len();
        assert!(request[..count].starts_with(b"GET /configs"));
        let body = r#"{"mixed-port":7890,"mode":"global"}"#;
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
        format!("http://{address}"),
        "",
    ))
    .unwrap();
    let session =
        CoreSession::open_with_config(CoreKind::Mihomo, client, Some(path), Vec::new()).unwrap();
    let commands = ApplicationCommands::new(session.clone(), Some(overrides));
    let other_entry_point = commands.clone();
    let failure = commands
        .activate(store.clone(), controlled.clone(), candidate.id.clone())
        .await
        .err()
        .unwrap();
    assert!(matches!(
        failure.retained_outcome.as_deref(),
        Some(ProfileApplyOutcome::RuntimeUnknown { .. })
    ));
    let recovery = other_entry_point.latest_recovery().unwrap();
    assert_eq!(recovery.attempted.profile_id, candidate.id);
    assert_eq!(
        recovery.last_known_good.as_ref().unwrap().profile_id,
        previous.id
    );
    assert!(
        other_entry_point
            .activate(store.clone(), controlled.clone(), "missing".into())
            .await
            .is_err()
    );
    assert_eq!(commands.latest_recovery(), Some(recovery));
    let accepted = Arc::new(
        ProfileApplication::new(store.clone(), controlled.clone(), session.clone())
            .apply(ProfileChange::ActivateExisting {
                id: previous.id.clone(),
                overrides: Vec::new(),
            })
            .await,
    );
    let ProfileApplyOutcome::Applied {
        runtime_version, ..
    } = accepted.as_ref()
    else {
        panic!("the recovery profile was not accepted");
    };
    let applied_version = *runtime_version;
    session.set_mode(&controlled, "global").await.unwrap();
    assert!(session.generation() > applied_version);
    let receipt = other_entry_point
        .finish_apply(accepted, ChangeContext::Selection, None)
        .unwrap();
    server.await.unwrap();
    assert_eq!(receipt.runtime_version(), Some(applied_version));
    assert!(commands.latest_recovery().is_none());
    assert_eq!(store.load().unwrap().active, Some(previous.id));
    std::fs::remove_dir_all(root).unwrap();
}
