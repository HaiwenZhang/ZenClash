use super::*;

#[tokio::test]
async fn service_handover_saved_unknown_retains_receipt_without_restoring_proxy() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    *backend.service_reply.lock().unwrap() = Some(
        crate::runtime::session::service_tun::ServiceTunRuntimeOutcome {
            saved: Some(crate::CoreApplyOutcome {
                kind: crate::CoreApplyKind::Patched,
                generation: 7,
            }),
            commit_pending: true,
            recovery_warning: None,
            failure: Some(crate::MihomoError::RuntimeOutcomeUnknown.into()),
            restored: false,
        },
    );
    let capture = TrafficCaptureSession::with_backend(backend.clone());
    let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
    assert_eq!(outcome.core().unwrap().generation, 7);
    assert!(outcome.commit_pending());
    assert!(outcome.recovery_warning().is_none());
    assert!(matches!(
        outcome.capture(),
        CaptureOutcome::ReconcileNeeded { .. }
    ));
    assert_eq!(
        backend.operations(),
        ["system-proxy:false", "service-handover"]
    );
}

#[tokio::test]
async fn service_handover_committed_capture_readback_failure_is_not_pending_commit() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.fail_next("capture-readback");
    let capture = TrafficCaptureSession::with_backend(backend);
    let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
    assert!(outcome.core().is_some());
    assert!(!outcome.commit_pending());
    let warning = TrafficCaptureError::Controller(crate::MihomoError::Api {
        status: 503,
        message: "capture-readback failed".into(),
    })
    .to_string();
    assert_eq!(outcome.recovery_warning(), Some(warning.as_str()));
    assert!(matches!(
        outcome.capture(),
        CaptureOutcome::ReconcileNeeded { .. }
    ));
}

#[tokio::test]
async fn service_handover_committed_cleanup_failure_keeps_only_recovery_warning() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
    *backend.service_reply.lock().unwrap() = Some(
        crate::runtime::session::service_tun::ServiceTunRuntimeOutcome {
            saved: Some(crate::CoreApplyOutcome {
                kind: crate::CoreApplyKind::Patched,
                generation: 7,
            }),
            commit_pending: false,
            recovery_warning: Some(warning.clone()),
            failure: Some(crate::CoreSessionError::PreviousCoreCleanupUnconfirmed),
            restored: false,
        },
    );
    let capture = TrafficCaptureSession::with_backend(backend);
    let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
    assert_eq!(outcome.core().unwrap().generation, 7);
    assert!(!outcome.commit_pending());
    assert_eq!(outcome.recovery_warning(), Some(warning.as_str()));
}

#[tokio::test]
async fn service_handover_unsaved_confirmed_restore_restores_owned_proxy() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    *backend.service_reply.lock().unwrap() = Some(
        crate::runtime::session::service_tun::ServiceTunRuntimeOutcome {
            saved: None,
            commit_pending: false,
            recovery_warning: None,
            failure: Some(
                crate::MihomoError::Process("injected preparation failure".into()).into(),
            ),
            restored: true,
        },
    );
    let capture = TrafficCaptureSession::with_backend(backend.clone());
    let outcome = capture.enable_service_tun(None, 0, 0).await.unwrap();
    assert!(outcome.core().is_none());
    assert!(matches!(
        outcome.capture(),
        CaptureOutcome::RolledBack { .. }
    ));
    assert_eq!(
        backend.operations(),
        [
            "system-proxy:false",
            "service-handover",
            "system-proxy:true"
        ]
    );
}

#[tokio::test]
async fn service_handover_waiter_cancel_keeps_shutdown_waiting_for_completion() {
    use crate::test_support::session::OwnedCoreFixture as ChildFixture;
    let child = ChildFixture::new("service-capture-shutdown").await;
    let core = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::from_process(child.process.clone()).unwrap(),
    )
    .unwrap();
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        false,
    ));
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    *backend.blocked_handover.lock().unwrap() = Some(BlockedWrite {
        entered,
        release: release.0.clone(),
    });
    let capture = TrafficCaptureSession {
        backend: backend.clone(),
        profile: Arc::default(),
        operation: core.capture_publication_gate(),
        intent_revision: core.capture_intent_revision(),
    };
    let waiter = tokio::spawn(async move { capture.enable_service_tun(None, 0, 0).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let closing = core.clone();
    let mut shutdown = tokio::spawn(async move { closing.shutdown().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut shutdown)
            .await
            .is_err()
    );
    assert!(child.process.is_running());
    drop(release);
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(backend.operations(), ["service-handover"]);
    assert!(!child.process.is_running());
}

#[tokio::test]
async fn enable_service_tun_cleanup_failure_preserves_original_rollback_error() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    *backend.service_error.lock().unwrap() = Some(crate::CoreSessionError::Process(
        crate::MihomoError::Process("service handover failed".into()),
    ));
    backend.fail_next("system-proxy:true");
    *backend.stop_unowned_error.lock().unwrap() =
        Some(crate::runtime::service::ipc::ServiceCallError::StartUnconfirmed);
    let session = TrafficCaptureSession::with_backend(backend.clone());
    let acquired = Arc::new(crate::runtime::service::ipc::ServiceSession::disconnected_for_test());

    let error = session
        .enable_service_tun_with_bundle(
            Some((acquired, PathBuf::from("/tmp"))),
            RuntimeConfigurationRevision {
                binding_generation: 0,
                configuration_generation: 0,
            },
            None,
            None,
            None,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(
            &error,
            crate::CoreSessionError::Capture(capture)
                if matches!(capture.as_ref(), TrafficCaptureError::Rollback { .. })
        ),
        "cleanup failure must not mask the original Rollback error, got: {error:?}"
    );
    assert_eq!(
        backend.operations(),
        [
            "system-proxy:false",
            "service-handover",
            "system-proxy:true",
            "stop-unowned-service",
        ]
    );
}
