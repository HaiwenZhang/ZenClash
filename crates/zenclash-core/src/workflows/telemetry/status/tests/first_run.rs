use super::*;

#[test]
fn first_run_resumes_from_domain_facts_without_a_persisted_wizard_step() {
    let mut snapshot = controllable_snapshot();
    assert_eq!(snapshot.first_run_stage(false), FirstRunStage::NoProfile);
    assert_eq!(
        snapshot.first_run_stage(true),
        FirstRunStage::CaptureNotSelected
    );

    let system_proxy = snapshot.capture.system_proxy.value().unwrap().clone();
    snapshot.capture.system_proxy = Observation::Fresh {
        value: SystemProxySessionSnapshot {
            intent_enabled: true,
            ..system_proxy
        },
        observed_at_ms: 2,
    };
    assert_eq!(
        snapshot.first_run_stage(true),
        FirstRunStage::CaptureUnconfirmed
    );

    let system_proxy = snapshot.capture.system_proxy.value().unwrap().clone();
    snapshot.capture.system_proxy = Observation::Fresh {
        value: SystemProxySessionSnapshot {
            actual: SystemProxyStatus {
                enabled: true,
                secure_enabled: true,
                port: 7890,
                secure_port: 7890,
                ..SystemProxyStatus::default()
            },
            ownership: SystemProxyOwnershipState::Owned,
            ..system_proxy
        },
        observed_at_ms: 3,
    };
    assert_eq!(snapshot.first_run_stage(true), FirstRunStage::PathUnknown);

    snapshot.path = Observation::Fresh {
        value: PathStatus {
            route: ObservedPathRoute::Mihomo,
            target: "https://example.com".into(),
            generation: 7,
        },
        observed_at_ms: 4,
    };
    assert_eq!(snapshot.first_run_stage(true), FirstRunStage::Ready);
}

#[test]
fn stale_or_foreign_generation_evidence_cannot_declare_ready() {
    let mut ready = controllable_snapshot();
    if let Observation::Fresh { value, .. } = &mut ready.capture.tun {
        value.requested = true;
        value.configured = true;
        value.observed = CapabilityState::Active;
    }
    ready.path = Observation::Fresh {
        value: PathStatus {
            route: ObservedPathRoute::Mihomo,
            target: "probe".into(),
            generation: 7,
        },
        observed_at_ms: 2,
    };
    assert_eq!(ready.first_run_stage(true), FirstRunStage::Ready);
    let mut stale_controller = ready.clone();
    stale_controller.controller = Observation::record(
        &stale_controller.controller,
        Err::<ControllerStatus, _>("controller failed"),
        3,
        RecoveryAction::InspectCore,
    );
    assert_eq!(
        stale_controller.first_run_stage(true),
        FirstRunStage::CoreUnavailable
    );
    let mut foreign_controller = ready.clone();
    if let Observation::Fresh { value, .. } = &mut foreign_controller.controller {
        value.generation = 6;
    }
    assert_eq!(
        foreign_controller.first_run_stage(true),
        FirstRunStage::CoreUnavailable
    );
    let mut stale_process = ready.clone();
    stale_process.process = Observation::record(
        &stale_process.process,
        Err::<ProcessStatus, _>("process failed"),
        3,
        RecoveryAction::InspectCore,
    );
    assert_eq!(
        stale_process.first_run_stage(true),
        FirstRunStage::CoreUnavailable
    );
    let mut stale_tun = ready;
    stale_tun.capture.tun = Observation::record(
        &stale_tun.capture.tun,
        Err::<TunCaptureStatus, _>("tun failed"),
        3,
        RecoveryAction::ReviewCapture,
    );
    assert!(!stale_tun.capture.is_active());
    assert_eq!(
        stale_tun.first_run_stage(true),
        FirstRunStage::CaptureUnconfirmed
    );
    assert!(
        stale_tun.capture.tun.value().is_some(),
        "retain last successful data for display"
    );
}

#[test]
fn stale_proxy_is_unconfirmed_but_independent_fresh_tun_still_proves_capture() {
    let mut snapshot = controllable_snapshot();
    snapshot.capture.system_proxy = Observation::Fresh {
        value: SystemProxySessionSnapshot {
            intent_enabled: true,
            actual: SystemProxyStatus {
                enabled: true,
                secure_enabled: true,
                port: 7890,
                secure_port: 7890,
                ..Default::default()
            },
            ownership: SystemProxyOwnershipState::Owned,
        },
        observed_at_ms: 1,
    };
    snapshot.capture.system_proxy = Observation::record(
        &snapshot.capture.system_proxy,
        Err::<SystemProxySessionSnapshot, _>("native read failed"),
        2,
        RecoveryAction::ReviewCapture,
    );
    assert!(
        snapshot
            .capture
            .system_proxy
            .value()
            .unwrap()
            .actual
            .active()
    );
    assert!(!snapshot.capture.is_active());
    assert_eq!(
        snapshot.first_run_stage(true),
        FirstRunStage::CaptureUnconfirmed
    );
    if let Observation::Fresh { value, .. } = &mut snapshot.capture.tun {
        value.observed = CapabilityState::Active;
    }
    assert!(snapshot.capture.is_active());
    assert_eq!(snapshot.first_run_stage(true), FirstRunStage::PathUnknown);
}

#[test]
fn old_generation_path_probe_cannot_advance_first_run_state() {
    let (sender, _) = watch::channel(controllable_snapshot());
    let status = OperationalStatus {
        snapshot: sender,
        task: std::sync::OnceLock::new(),
        wake: Arc::new(tokio::sync::Notify::new()),
        core_session: None,
    };

    assert!(!status.record_path(
        6,
        Ok(PathStatus {
            route: ObservedPathRoute::Mihomo,
            target: "https://example.com".into(),
            generation: 6,
        })
    ));
    assert!(matches!(status.snapshot().path, Observation::Loading));
}

#[test]
fn capture_status_is_active_requires_owned_system_proxy() {
    let active_proxy = SystemProxyStatus {
        enabled: true,
        secure_enabled: true,
        port: 7890,
        secure_port: 7890,
        ..SystemProxyStatus::default()
    };
    for ownership in [
        SystemProxyOwnershipState::Unowned,
        SystemProxyOwnershipState::Lost,
    ] {
        let capture = CaptureStatus {
            system_proxy: Observation::Fresh {
                value: SystemProxySessionSnapshot {
                    intent_enabled: false,
                    actual: active_proxy.clone(),
                    ownership,
                },
                observed_at_ms: 1,
            },
            tun: Observation::Loading,
        };
        assert!(
            !capture.is_active(),
            "non-owned system proxy ({ownership:?}) must not be treated as active ZenClash capture"
        );
    }

    let owned_capture = CaptureStatus {
        system_proxy: Observation::Fresh {
            value: SystemProxySessionSnapshot {
                intent_enabled: true,
                actual: active_proxy,
                ownership: SystemProxyOwnershipState::Owned,
            },
            observed_at_ms: 1,
        },
        tun: Observation::Loading,
    };
    assert!(owned_capture.is_active());
}

#[test]
fn record_path_rejects_generation_zero_when_process_is_not_ready() {
    let (sender, _) = watch::channel(OperationalSnapshot::default());
    let status = OperationalStatus {
        snapshot: sender,
        task: std::sync::OnceLock::new(),
        wake: Arc::new(tokio::sync::Notify::new()),
        core_session: None,
    };
    assert!(!status.record_path(
        0,
        Ok(PathStatus {
            route: ObservedPathRoute::Mihomo,
            target: "https://example.com".into(),
            generation: 0,
        }),
    ));
    assert_eq!(status.snapshot().path, Observation::Loading);
}
