use super::*;

#[tokio::test]
async fn system_proxy_failure_restores_the_previous_tun_state() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.fail_next("system-proxy:true");
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

    assert!(matches!(outcome, CaptureOutcome::RolledBack { .. }));
    assert_eq!(
        backend.operations(),
        ["tun:false", "system-proxy:true", "tun:true"]
    );
    assert_eq!(
        outcome.snapshot().observed_plan,
        ObservedCapturePlan::TunConfigured
    );
}

#[tokio::test]
async fn system_proxy_resynchronizes_the_runtime_when_the_initial_port_is_missing() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        false,
    ));
    backend.state.lock().unwrap().system_proxy_port = None;
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

    assert!(matches!(
        outcome,
        CaptureOutcome::Applied {
            plan: CapturePlan::SystemProxy,
            ..
        }
    ));
    assert_eq!(
        backend.operations(),
        ["resync-runtime-profile", "system-proxy:true"]
    );
}

#[tokio::test]
async fn system_proxy_keeps_the_safe_error_when_resynchronization_has_no_port() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        false,
    ));
    {
        let mut state = backend.state.lock().unwrap();
        state.system_proxy_port = None;
        state.resynced_system_proxy_port = None;
    }
    let session = TrafficCaptureSession::with_backend(backend.clone());

    let error = session
        .apply(CapturePlan::SystemProxy)
        .await
        .expect_err("a profile without HTTP/Mixed listeners remains unsafe");

    assert!(error.to_string().contains(&zenclash_i18n::text(
        "traffic_capture.errors.missing_system_proxy_port"
    )));
    assert_eq!(backend.operations(), ["resync-runtime-profile"]);
}

#[tokio::test]
async fn failed_rollback_exposes_reconcile_needed() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.fail_next("system-proxy:true");
    backend.fail_next("tun:true");
    let session = TrafficCaptureSession::with_backend(backend);

    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

    assert!(matches!(
        outcome,
        CaptureOutcome::ReconcileNeeded {
            plan: Some(CapturePlan::SystemProxy),
            ..
        }
    ));
}

#[tokio::test]
async fn proxy_io_failure_survives_capture_and_core_error_conversion() {
    use std::{error::Error, io};
    let backend = Arc::new(FakeBackend::new(
        true,
        SystemProxyOwnershipState::Owned,
        false,
    ));
    backend.state.lock().unwrap().proxy_failure = Some(crate::SystemProxySessionError::Native(
        crate::SystemProxyError::PlatformIo {
            backend: crate::SystemProxyBackendKind::MacOs,
            operation: "fixture_native_write",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "fixture write denied"),
        }
        .into(),
    ));
    let session = TrafficCaptureSession::with_backend(backend.clone());
    let error = session.apply(CapturePlan::Off).await.unwrap_err();
    assert!(
        matches!(&error, TrafficCaptureError::SystemProxy(crate::SystemProxySessionError::Native(
        crate::MihomoError::SystemProxy(error)))
        if matches!(error.as_ref(), crate::SystemProxyError::PlatformIo { operation: "fixture_native_write", .. }))
    );
    let core_error = crate::CoreSessionError::from(error);
    assert!(matches!(&core_error, crate::CoreSessionError::Capture(_)));
    assert_eq!(
        core_error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(backend.operations(), ["system-proxy:false"]);
}

#[tokio::test]
async fn native_recovery_uncertainty_requires_reconciliation_after_tun_rollback() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.state.lock().unwrap().proxy_failure =
        Some(crate::SystemProxySessionError::Transaction(Box::new(
            crate::SystemProxyError::RecoveryUnconfirmed {
                cause: Box::new(crate::SystemProxyTransactionCause::Preferences(
                    crate::AppPreferencesError::ConcurrentModification,
                )),
                recovery: Box::new(crate::SystemProxyError::VerificationFailed.into()),
                release_failure: None,
            },
        )));
    let session = TrafficCaptureSession::with_backend(backend.clone());
    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();
    assert!(matches!(
        outcome,
        CaptureOutcome::ReconcileNeeded {
            plan: Some(CapturePlan::SystemProxy),
            ..
        }
    ));
    assert!(backend.state.lock().unwrap().tun_configured);
    assert_eq!(
        backend.operations(),
        ["tun:false", "system-proxy:true", "tun:true"]
    );
}

#[tokio::test]
async fn confirmed_native_recovery_can_report_capture_rollback() {
    let backend = Arc::new(FakeBackend::new(
        false,
        SystemProxyOwnershipState::Unowned,
        true,
    ));
    backend.state.lock().unwrap().proxy_failure =
        Some(crate::SystemProxySessionError::Transaction(Box::new(
            crate::SystemProxyError::RecoveryConfirmed {
                cause: Box::new(crate::SystemProxyTransactionCause::Preferences(
                    crate::AppPreferencesError::ConcurrentModification,
                )),
            },
        )));
    let session = TrafficCaptureSession::with_backend(backend.clone());
    let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();
    assert!(matches!(
        outcome,
        CaptureOutcome::RolledBack {
            plan: CapturePlan::SystemProxy,
            ..
        }
    ));
    assert!(backend.state.lock().unwrap().tun_configured);
    assert_eq!(
        backend.operations(),
        ["tun:false", "system-proxy:true", "tun:true"]
    );
}
