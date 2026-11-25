use super::*;
use crate::runtime::service::ipc::NativeHttpError;
use std::sync::atomic::{AtomicBool, Ordering};
use zenclash_service::ServiceLifecycleState;

fn status_snapshot(
    is_active: bool,
    active_generation: Option<u64>,
    core_pid: Option<u32>,
) -> ServiceStatusSnapshot {
    ServiceStatusSnapshot {
        is_active,
        active_generation,
        service_state: if is_active {
            ServiceLifecycleState::Running
        } else {
            ServiceLifecycleState::Starting
        },
        core_pid,
        core_started_at: None,
        last_core_exit_reason: None,
        restart_count: 0,
        last_recovery_at: None,
        desired_core_should_be_running: is_active,
        desired_generation: active_generation.unwrap_or(0),
        desired_updated_at: 0,
    }
}

fn dummy_runtime_bundle() -> RuntimeBundle {
    RuntimeBundle {
        yaml: "mode: rule\n".into(),
        assets: Vec::new(),
        remote_providers: Vec::new(),
        core_path: String::new(),
    }
}

#[test]
fn macos_lan_dns_is_replaced_only_for_confirmed_udp_tun_capture() {
    assert_eq!(
        macos_tun_dns_required(&serde_json::json!({"tun": {
            "enable": true, "auto-route": true, "dns-hijack": ["any:53"]
        }})),
        Some(true)
    );
    for tun in [
        serde_json::json!({"enable": false}),
        serde_json::json!({"enable": true, "auto-route": false, "dns-hijack": ["any:53"]}),
        serde_json::json!({"enable": true, "dns-hijack": []}),
        serde_json::json!({"enable": true, "dns-hijack": ["tcp://any:53"]}),
    ] {
        assert_eq!(
            macos_tun_dns_required(&serde_json::json!({"tun": tun})),
            Some(false)
        );
    }
    assert_eq!(macos_tun_dns_required(&serde_json::json!({})), None);
}

#[test]
fn read_only_get_configs_does_not_require_mutation_lock() {
    assert!(!controller_request_requires_mutation_lock(
        "GET", "/configs"
    ));
    assert!(!controller_request_requires_mutation_lock(
        "HEAD", "/configs"
    ));
    assert!(!controller_request_requires_mutation_lock(
        "GET",
        "/configs?force=true"
    ));
    assert!(!controller_request_requires_mutation_lock(
        "PUT", "/proxies"
    ));
    assert!(controller_request_requires_mutation_lock("PUT", "/configs"));
    assert!(controller_request_requires_mutation_lock(
        "PUT",
        "/configs?force=true"
    ));
    assert!(controller_request_requires_mutation_lock(
        "PATCH", "/configs"
    ));
}

#[tokio::test]
async fn explicit_sync_tun_dns_applies_and_confirms_owner_and_propagates_errors() {
    let session = ServiceSession::disconnected_for_test();
    let proof = OwnerSessionProof {
        generation: 7,
        token: "tok".into(),
    };
    let applied = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let confirmed = Arc::new(AtomicBool::new(false));
    let applied_ref = applied.clone();
    let confirmed_ref = confirmed.clone();
    let proof_copy = proof.clone();
    session
        .sync_tun_dns_with(
            || async move {
                Ok((
                    proof_copy,
                    42,
                    crate::runtime::service::ipc::NativeHttpResponse {
                        status: 200,
                        body:
                            br#"{"tun":{"enable":true,"auto-route":true,"dns-hijack":["any:53"]}}"#
                                .to_vec(),
                    },
                ))
            },
            |proof, enabled| async move {
                applied_ref.lock().push((proof.generation, enabled));
                Ok(())
            },
            |proof, pid| async move {
                assert_eq!(proof.generation, 7);
                assert_eq!(pid, 42);
                confirmed_ref.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(*applied.lock(), vec![(7, true)]);
    assert!(confirmed.load(Ordering::SeqCst));

    // Failure in privileged DNS sync must propagate instead of succeeding silently.
    let proof_copy = proof.clone();
    let error = session
        .sync_tun_dns_with(
            || async move {
                Ok((
                    proof_copy,
                    42,
                    crate::runtime::service::ipc::NativeHttpResponse {
                        status: 200,
                        body: br#"{"tun":{"enable":false}}"#.to_vec(),
                    },
                ))
            },
            |_proof, enabled| async move {
                assert!(!enabled);
                Err(ServiceCallError::Rejected {
                    code: ServiceErrorCode::ProxyApplyFailed as u16,
                    message: "dns write failed".into(),
                })
            },
            |_proof, _pid| async { panic!("confirm_owner must not run after DNS failure") },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ServiceCallError::Rejected { .. }));
}

#[tokio::test]
async fn stop_clears_pending_start_and_aborts_keepalive_when_service_is_inactive() {
    let session = ServiceSession::disconnected_for_test();
    let lost = session
        .start_exchange(dummy_runtime_bundle(), |_request| async {
            Err(ServiceCallError::Transport(anyhow::anyhow!(
                "start response timed out"
            )))
        })
        .await
        .unwrap_err();
    assert!(lost.mutation_result_unknown());
    assert!(matches!(
        session.active_proof(),
        Err(ServiceCallError::StartUnconfirmed)
    ));
    assert!(session.pending_start.read().is_some());
    assert!(session.pending_keepalive.read().is_some());

    // Service actually reports inactive: stop() must clear pending state & keepalive.
    let stop_called = Arc::new(AtomicBool::new(false));
    let stop_called_ref = stop_called.clone();
    session
        .stop_with(
            || async { Ok(status_snapshot(false, None, None)) },
            |_proof| async move {
                stop_called_ref.store(true, Ordering::SeqCst);
                Ok((0, String::new()))
            },
        )
        .await
        .unwrap();
    assert!(!stop_called.load(Ordering::SeqCst));
    assert!(session.pending_start.read().is_none());
    assert!(session.pending_keepalive.read().is_none());
    assert!(matches!(
        session.active_proof(),
        Err(ServiceCallError::NoActiveSession)
    ));

    // Subsequent start must no longer be permanently blocked by StartUnconfirmed.
    let reached_exchange = Arc::new(AtomicBool::new(false));
    let reached_ref = reached_exchange.clone();
    let err = session
        .start_exchange(dummy_runtime_bundle(), |_request| async move {
            reached_ref.store(true, Ordering::SeqCst);
            Err(ServiceCallError::Rejected {
                code: ServiceErrorCode::InvalidRuntimeAsset as u16,
                message: "definitive refusal".into(),
            })
        })
        .await
        .unwrap_err();
    assert!(reached_exchange.load(Ordering::SeqCst));
    assert!(matches!(err, ServiceCallError::Rejected { .. }));
}

#[tokio::test]
async fn stop_preserves_pending_start_when_replacement_rejects_with_stale_owner() {
    let session = ServiceSession::disconnected_for_test();
    session
        .start_exchange(dummy_runtime_bundle(), |_request| async {
            Err(ServiceCallError::Transport(anyhow::anyhow!("timeout")))
        })
        .await
        .unwrap_err();

    let err = session
        .stop_with(
            || async { Ok(status_snapshot(true, Some(9), Some(999))) },
            |proof| async move {
                assert_eq!(proof.generation, 9);
                Ok((
                    ServiceErrorCode::StaleOwnerSession as u16,
                    "replacement owns session".into(),
                ))
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ServiceCallError::Rejected { code, .. }
            if code == ServiceErrorCode::StaleOwnerSession as u16
    ));
    assert!(matches!(
        session.active_proof(),
        Err(ServiceCallError::StartUnconfirmed)
    ));
}

#[tokio::test]
async fn retire_confirmed_stop_clears_pending_state_when_service_confirmed_inactive() {
    let session = ServiceSession::disconnected_for_test();
    session
        .start_exchange(dummy_runtime_bundle(), |_request| async {
            Err(ServiceCallError::Transport(anyhow::anyhow!("timeout")))
        })
        .await
        .unwrap_err();
    assert!(session.pending_start.read().is_some());
    assert!(session.pending_keepalive.read().is_some());

    session.retire_confirmed_stop(None, &status_snapshot(false, None, None));

    assert!(session.pending_start.read().is_none());
    assert!(session.pending_keepalive.read().is_none());
    assert!(matches!(
        session.active_proof(),
        Err(ServiceCallError::NoActiveSession)
    ));
}

#[test]
fn controller_failure_keeps_the_send_boundary() {
    for (error, expected) in [
        (
            NativeHttpError::BeforeSend(std::io::Error::other("peer mismatch")),
            false,
        ),
        (
            NativeHttpError::OutcomeUnknown(std::io::Error::other("response lost")),
            true,
        ),
        (
            NativeHttpError::BudgetExceeded {
                request_sent: false,
            },
            false,
        ),
        (NativeHttpError::BudgetExceeded { request_sent: true }, true),
    ] {
        assert_eq!(
            ServiceCallError::Controller(error).mutation_result_unknown(),
            expected
        );
    }
}

#[test]
fn protocol_and_input_refusals_are_definitive_but_partial_failures_are_unknown() {
    for (code, unknown) in [
        (ServiceErrorCode::UnauthorizedOwner, false),
        (ServiceErrorCode::InvalidRuntimeAsset, false),
        (ServiceErrorCode::ProtocolMismatch, false),
        (ServiceErrorCode::StaleOwnerSession, false),
        (ServiceErrorCode::OwnerSwitchFailed, true),
        (ServiceErrorCode::ProxyClearFailed, true),
        (ServiceErrorCode::ProxyApplyFailed, true),
    ] {
        assert_eq!(
            ServiceCallError::Rejected {
                code: code as u16,
                message: String::new()
            }
            .mutation_result_unknown(),
            unknown
        );
    }
    assert!(ServiceCallError::OwnerLost.mutation_result_unknown());
}
