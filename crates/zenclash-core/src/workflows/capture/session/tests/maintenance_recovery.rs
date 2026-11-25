use super::*;

#[tokio::test]
async fn maintenance_tun_admission_rejects_new_capture_choice_before_handover() {
    let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
    let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
    let revision = capture.capture_revision();
    capture.apply(CapturePlan::Off).await.unwrap();
    let before = backend.operations();
    let result = capture
        .enable_service_tun_with_bundle(
            None,
            crate::runtime::session::RuntimeConfigurationRevision {
                binding_generation: 0,
                configuration_generation: 0,
            },
            None,
            Some(revision),
            None,
        )
        .await;
    assert!(matches!(
        result,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert_eq!(backend.operations(), before);
}

#[tokio::test]
async fn maintenance_proxy_restores_owned_advanced_capture_using_current_listener() {
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
    let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
    let before = capture.snapshot_from_backend().await.unwrap();
    backend.write_proxy(false, 0).unwrap();
    backend.state.lock().unwrap().system_proxy_port = Some(9090);
    capture
        .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
        .await
        .unwrap();
    let after = capture.snapshot_from_backend().await.unwrap();
    assert_eq!(after.observed_plan, ObservedCapturePlan::Advanced);
    assert_eq!(after.system_proxy.value().unwrap().actual.port, 9090);
    assert_eq!(
        backend.operations(),
        ["system-proxy:false", "system-proxy:true"]
    );
}

#[tokio::test]
async fn maintenance_proxy_preserves_external_or_unavailable_current_capture() {
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
    let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
    let before = capture.snapshot_from_backend().await.unwrap();
    backend.state.lock().unwrap().system_proxy.ownership = SystemProxyOwnershipState::Lost;
    assert!(matches!(
        capture
            .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
            .await,
        Err(TrafficCaptureError::ExternalSystemProxy)
    ));
    assert!(backend.operations().is_empty());
    backend.write_proxy(false, 0).unwrap();
    backend.set_core_available(false);
    assert!(
        capture
            .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
            .await
            .is_err()
    );
    assert_eq!(backend.operations(), ["system-proxy:false"]);
}

#[tokio::test]
async fn maintenance_proxy_never_acquires_unowned_or_unknown_previous_capture() {
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Lost, true);
    let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
    let before = capture.snapshot_from_backend().await.unwrap();
    capture
        .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
        .await
        .unwrap();
    assert!(
        capture
            .restore_maintenance_proxy_admitted(
                &failed_snapshot("unknown".into()),
                capture.capture_revision()
            )
            .await
            .is_err()
    );
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn maintenance_proxy_rejects_zero_listener_without_native_write() {
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
    let capture = TrafficCaptureSession::with_backend(Arc::new(backend.clone()));
    let before = capture.snapshot_from_backend().await.unwrap();
    backend.write_proxy(false, 0).unwrap();
    backend.state.lock().unwrap().system_proxy_port = Some(0);
    assert!(
        capture
            .restore_maintenance_proxy_admitted(&before, capture.capture_revision())
            .await
            .is_err()
    );
    assert_eq!(backend.operations(), ["system-proxy:false"]);
}

#[tokio::test]
async fn maintenance_proxy_rejects_new_capture_intent_without_core_generation_change() {
    let core = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: core.capture_publication_gate(),
        intent_revision: core.capture_intent_revision(),
    };
    let before = capture.snapshot_from_backend().await.unwrap();
    backend.write_proxy(false, 0).unwrap();
    let generation = core.generation();
    let revision = capture.capture_revision();
    capture.apply(CapturePlan::SystemProxy).await.unwrap();
    assert_eq!(core.generation(), generation);
    let count = backend.operations().len();
    assert!(matches!(
        capture
            .restore_maintenance_proxy(
                &core,
                before,
                RuntimeConfigurationRevision {
                    binding_generation: 0,
                    configuration_generation: generation
                },
                revision
            )
            .await,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert_eq!(backend.operations().len(), count);
    assert!(backend.state.lock().unwrap().system_proxy.intent_enabled);
}

#[tokio::test]
async fn maintenance_proxy_rejects_replaced_runtime_without_using_new_version() {
    let core = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, false);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: core.capture_publication_gate(),
        intent_revision: core.capture_intent_revision(),
    };
    let before = capture.snapshot_from_backend().await.unwrap();
    core.switch_to_direct(crate::MihomoEndpoint {
        secret: "replacement-maintenance-fixture".into(),
        ..crate::MihomoEndpoint::default()
    })
    .await
    .unwrap();
    assert!(matches!(
        capture
            .restore_maintenance_proxy(
                &core,
                before,
                RuntimeConfigurationRevision {
                    binding_generation: 0,
                    configuration_generation: 0
                },
                0
            )
            .await,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn maintenance_proxy_waiter_cancellation_keeps_shutdown_waiting_for_write() {
    let core = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: core.capture_publication_gate(),
        intent_revision: core.capture_intent_revision(),
    };
    let before = capture.snapshot_from_backend().await.unwrap();
    backend.write_proxy(false, 0).unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    *backend.blocked_write.lock().unwrap() = Some(BlockedWrite {
        entered,
        release: release.0.clone(),
    });
    let session = core.clone();
    let task = tokio::spawn(async move {
        capture
            .restore_maintenance_proxy(
                &session,
                before,
                RuntimeConfigurationRevision {
                    binding_generation: 0,
                    configuration_generation: 0,
                },
                0,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let session = core.clone();
    let mut shutdown = tokio::spawn(async move { session.shutdown().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut shutdown)
            .await
            .is_err()
    );
    drop(release);
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        backend.operations(),
        ["system-proxy:false", "system-proxy:true"]
    );
    assert!(backend.state.lock().unwrap().tun_configured);
}

#[tokio::test]
async fn cancelled_capture_queue_does_not_start_native_work_after_admission_releases() {
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: session.capture_publication_gate(),
        intent_revision: session.capture_intent_revision(),
    };
    let held = session.capture_publication_gate().lock_owned().await;
    let queued = tokio::spawn(async move { capture.apply(CapturePlan::SystemProxy).await });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
    drop(held);
    tokio::task::yield_now().await;
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn service_recovery_queue_never_adopts_a_new_capture_intent() {
    let core = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, false);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: core.capture_publication_gate(),
        intent_revision: core.capture_intent_revision(),
    };
    let held = core.capture_publication_gate().lock_owned().await;
    let queued_capture = capture.clone();
    let session = core.clone();
    let (polled, waiting) = tokio::sync::oneshot::channel();
    let mut polled = Some(polled);
    let recovery = tokio::spawn(async move {
        let store =
            ControlledConfigStore::new(std::env::temp_dir().join("unused-capture-queue-store"));
        let mut future =
            Box::pin(queued_capture.recover_service_to_local(&session, &store, None, 0, 0));
        std::future::poll_fn(|cx| {
            let result = future.as_mut().poll(cx);
            if result.is_pending()
                && let Some(polled) = polled.take()
            {
                let _ = polled.send(());
            }
            result
        })
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    let applying = capture.clone();
    let apply = tokio::spawn(async move { applying.apply(CapturePlan::Off).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while capture.capture_revision() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    apply.abort();
    assert!(apply.await.unwrap_err().is_cancelled());
    drop(held);
    assert!(matches!(
        recovery.await.unwrap(),
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert!(backend.operations().is_empty());
    assert_eq!(core.generation(), 0);
}

#[tokio::test]
async fn service_recovery_cancelled_in_capture_queue_performs_no_native_release() {
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: session.capture_publication_gate(),
        intent_revision: session.capture_intent_revision(),
    };
    let binding = session.runtime_descriptor().binding_generation();
    let generation = session.generation();
    let held = session.capture_publication_gate().lock_owned().await;
    let queued = tokio::spawn(async move {
        let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
        capture
            .recover_service_to_local(&session, &store, None, binding, generation)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
    drop(held);
    tokio::task::yield_now().await;
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn service_recovery_rechecks_generation_after_capture_queue() {
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: session.capture_publication_gate(),
        intent_revision: session.capture_intent_revision(),
    };
    let binding = session.runtime_descriptor().binding_generation();
    let generation = session.generation();
    let held = session.capture_publication_gate().lock_owned().await;
    let queued_session = session.clone();
    let queued = tokio::spawn(async move {
        let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
        capture
            .recover_service_to_local(&queued_session, &store, None, binding, generation)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    session.invalidate_runtime_observations();
    drop(held);
    assert!(matches!(
        queued.await.unwrap(),
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn service_recovery_rejects_another_core_before_capture_admission() {
    let open = || {
        CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap()
    };
    let session = open();
    let other = open();
    let backend = FakeBackend::new(true, SystemProxyOwnershipState::Owned, true);
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: session.capture_publication_gate(),
        intent_revision: session.capture_intent_revision(),
    };
    let _held = session.capture_publication_gate().lock_owned().await;
    let store = ControlledConfigStore::new(std::env::temp_dir().join("unused-recovery"));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        capture.recover_service_to_local(
            &other,
            &store,
            None,
            other.runtime_descriptor().binding_generation(),
            other.generation(),
        ),
    )
    .await
    .expect("mismatched owner must not wait on capture");
    assert!(matches!(
        result,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::StaleBinding
        ))
    ));
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn owner_switch_waits_for_admitted_capture_native_write() {
    exercise_capture_publication(false, false).await;
}

#[tokio::test]
async fn owner_switch_waits_for_admitted_capture_rollback() {
    exercise_capture_publication(false, true).await;
}

#[tokio::test]
async fn cancelled_capture_waiter_keeps_publication_blocked_until_completion() {
    exercise_capture_publication(true, false).await;
}

async fn exercise_capture_publication(cancel_waiter: bool, fail_native: bool) {
    use crate::test_support::session::OwnedCoreFixture as ChildFixture;
    let first = ChildFixture::new("capture-publication-first").await;
    let second = ChildFixture::new("capture-publication-second").await;
    let session = CoreSession::open(
        crate::CoreKind::Mihomo,
        crate::MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    let backend = FakeBackend::new(false, SystemProxyOwnershipState::Unowned, fail_native);
    if fail_native {
        backend.fail_next("system-proxy:true");
    }
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let release = ReleaseWrite(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    *backend.blocked_write.lock().unwrap() = Some(BlockedWrite {
        entered,
        release: release.0.clone(),
    });
    // Real child retirement with a bounded blocking native-side-effect fixture;
    // this does not modify the operating system's proxy settings.
    let capture = TrafficCaptureSession {
        backend: Arc::new(backend.clone()),
        profile: Arc::default(),
        operation: session.capture_publication_gate(),
        intent_revision: session.capture_intent_revision(),
    };
    let apply = tokio::spawn(async move { capture.apply(CapturePlan::SystemProxy).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    if cancel_waiter {
        apply.abort();
    }
    let switch_session = session.clone();
    let replacement = second.process.clone();
    let mut switch =
        tokio::spawn(async move { switch_session.switch_to_process(replacement).await });
    let early = tokio::time::timeout(std::time::Duration::from_millis(100), &mut switch).await;
    let old_running_during_write = first.process.is_running();
    drop(release);
    assert!(
        early.is_err(),
        "owner publication completed while the old native write was paused"
    );
    assert!(
        old_running_during_write,
        "retirement stopped the child targeted by an unfinished native write"
    );
    if cancel_waiter {
        assert!(apply.await.unwrap_err().is_cancelled());
    } else {
        let outcome = apply.await.unwrap().unwrap();
        assert!(if fail_native {
            matches!(outcome, CaptureOutcome::RolledBack { .. })
        } else {
            matches!(outcome, CaptureOutcome::Applied { .. })
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), switch)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!first.process.is_running());
    assert!(second.process.is_running());
    if fail_native {
        assert!(
            backend.state.lock().unwrap().tun_configured,
            "publication escaped before TUN rollback"
        );
    }
    session.shutdown().await.unwrap();
}
