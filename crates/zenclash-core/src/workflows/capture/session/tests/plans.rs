use super::*;

#[tokio::test]
async fn reconcile_preserves_an_advanced_combination() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        true,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.reconcile().await.unwrap();

    assert_eq!(
        outcome.snapshot().observed_plan,
        ObservedCapturePlan::Advanced
    );
    assert_eq!(backend.operations(), ["reconcile"]);
}

#[tokio::test]
async fn a_tun_disable_not_confirmed_by_readback_cannot_enable_system_proxy() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.state.lock().unwrap().ignore_tun_writes = true;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

    assert!(matches!(outcome, CaptureOutcome::ReconcileNeeded { .. }));
    assert!(outcome.snapshot().tun.value().unwrap().configured);
    assert!(!backend.state.lock().unwrap().system_proxy.actual.active());
}

#[tokio::test]
async fn connection_cleanup_failure_does_not_restore_disabled_capture() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        true,
    ));
    backend.state.lock().unwrap().open_connections = 1;
    backend.fail_next("close-connections");
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::Off).await.unwrap();

    assert!(matches!(outcome, CaptureOutcome::ReconcileNeeded { .. }));
    assert_eq!(outcome.snapshot().observed_plan, ObservedCapturePlan::Off);
    assert_eq!(backend.state.lock().unwrap().open_connections, 1);
    assert_eq!(
        backend.operations(),
        ["system-proxy:false", "tun:false", "close-connections"]
    );
}

#[tokio::test]
async fn connection_cleanup_never_contacts_an_external_controller() {
    let client =
        crate::MihomoClient::new(crate::MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap();
    let core_session = CoreSession::open(crate::CoreKind::Mihomo, client).unwrap();
    let backend = ProductionCaptureBackend::new(
        core_session,
        ControlledConfigStore::new(
            std::env::temp_dir().join("zenclash-external-connection-cleanup"),
        ),
        None,
        Arc::default(),
    );

    // An attempted request would fail against this unreachable external controller.
    backend.close_owned_connections().await.unwrap();
}

#[tokio::test]
async fn turning_capture_off_closes_existing_connections_without_stopping_core() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    backend.state.lock().unwrap().open_connections = 1;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::Off).await.unwrap();

    assert_eq!(outcome.snapshot().observed_plan, ObservedCapturePlan::Off);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.open_connections, 0);
    assert!(state.core_available);
}

#[tokio::test]
async fn switching_from_tun_to_system_proxy_then_off_releases_both_capture_modes() {
    // Native capture is simulated so this regression cannot change host routing.
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.state.lock().unwrap().open_connections = 1;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let proxy = session.apply(CapturePlan::SystemProxy).await.unwrap();
    assert_eq!(
        proxy.snapshot().observed_plan,
        ObservedCapturePlan::SystemProxy
    );
    assert!(!proxy.snapshot().tun.value().unwrap().configured);
    assert_eq!(backend.state.lock().unwrap().open_connections, 1);

    backend.state.lock().unwrap().open_connections = 1;
    let off = session.apply(CapturePlan::Off).await.unwrap();
    assert_eq!(off.snapshot().observed_plan, ObservedCapturePlan::Off);
    let system_proxy = off.snapshot().system_proxy.value().unwrap();
    assert!(!system_proxy.intent_enabled);
    assert!(!system_proxy.actual.active());
    assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
    assert!(!off.snapshot().tun.value().unwrap().configured);
    assert_eq!(backend.state.lock().unwrap().open_connections, 0);
}

#[tokio::test]
async fn tun_off_then_on_does_not_clear_unrelated_connections() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.state.lock().unwrap().open_connections = 1;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let off = session.apply(CapturePlan::Off).await.unwrap();
    assert_eq!(off.snapshot().observed_plan, ObservedCapturePlan::Off);
    let on = session.apply(CapturePlan::Tun).await.unwrap();
    assert_eq!(
        on.snapshot().observed_plan,
        ObservedCapturePlan::TunConfigured
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.open_connections, 1);
    assert!(state.core_available);
}

#[tokio::test]
async fn switching_from_system_proxy_to_tun_closes_old_connections_and_preserves_core() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    backend.state.lock().unwrap().open_connections = 1;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::Tun).await.unwrap();

    assert_eq!(
        outcome.snapshot().observed_plan,
        ObservedCapturePlan::TunConfigured
    );
    assert!(
        !outcome
            .snapshot()
            .system_proxy
            .value()
            .unwrap()
            .actual
            .active()
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.open_connections, 0);
    assert!(state.core_available);
}

#[tokio::test]
async fn external_system_proxy_is_not_overwritten_by_off_or_tun() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Lost,
        false,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let result = session.apply(CapturePlan::Off).await;

    assert!(matches!(
        result,
        Err(TrafficCaptureError::ExternalSystemProxy)
    ));
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn rejected_permission_performs_no_capture_write() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        false,
    ));
    backend.state.lock().unwrap().permission_error = Some("permission rejected".into());
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::Tun).await;

    assert!(matches!(
        outcome,
        Err(TrafficCaptureError::Controller(crate::MihomoError::InvalidInput(message)))
            if message == "permission rejected"
    ));
    assert_eq!(backend.permission_requests(), 1);
    assert!(backend.operations().is_empty());
}

#[tokio::test]
async fn permission_prompt_is_reachable_only_from_an_explicit_tun_plan() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        false,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    session.reconcile().await.unwrap();
    session.release_owned().await.unwrap();
    session.apply(CapturePlan::SystemProxy).await.unwrap();
    session.apply(CapturePlan::Off).await.unwrap();

    assert_eq!(backend.permission_requests(), 0);
    session.apply(CapturePlan::Tun).await.unwrap();
    assert_eq!(backend.permission_requests(), 1);
}

#[tokio::test]
async fn normal_exit_releases_owned_system_proxy_without_changing_intent() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.release_owned().await.unwrap();

    let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
    assert!(system_proxy.intent_enabled);
    assert!(!system_proxy.actual.active());
    assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
    assert_eq!(backend.operations(), ["release-owned"]);
}

#[tokio::test]
async fn exit_does_not_overwrite_an_external_system_proxy_replacement() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Lost,
        false,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.release_owned().await.unwrap();

    let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
    assert!(system_proxy.actual.active());
    assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
    assert_eq!(backend.operations(), ["release-owned"]);
}

#[tokio::test]
async fn core_crash_releases_owned_proxy_and_restart_restores_persistent_intent() {
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());

    backend.set_core_available(false);
    let crashed = session.reconcile().await.unwrap();
    let crashed_proxy = crashed.snapshot().system_proxy.value().unwrap();
    assert!(crashed_proxy.intent_enabled);
    assert!(!crashed_proxy.actual.active());
    assert_eq!(crashed_proxy.ownership, SystemProxyOwnershipState::Unowned);

    backend.set_core_available(true);
    let restarted = session.reconcile().await.unwrap();
    let restarted_proxy = restarted.snapshot().system_proxy.value().unwrap();
    assert!(restarted_proxy.actual.active());
    assert_eq!(restarted_proxy.ownership, SystemProxyOwnershipState::Owned);
    assert_eq!(backend.operations(), ["reconcile", "reconcile"]);
}
