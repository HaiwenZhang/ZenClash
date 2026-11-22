use std::time::Duration;

use super::*;
use crate::{
    CoreKind, CoreSessionSnapshot, RuntimeConfig, SystemProxyOwnershipState,
    SystemProxySessionSnapshot, SystemProxyStatus, VersionInfo,
};

mod first_run;
mod refresh;

fn controllable_snapshot() -> OperationalSnapshot {
    OperationalSnapshot {
        process: Observation::Fresh {
            value: ProcessStatus {
                kind: CoreKind::Mihomo,
                managed: true,
                pid: None,
                started_at_secs: None,
                running: true,
                generation: 7,
                exit_reason: None,
                recovery_attempts: 0,
                recovery: ProcessRecoveryStatus::Stable,
            },
            observed_at_ms: 1,
        },
        controller: Observation::Fresh {
            value: ControllerStatus {
                version: VersionInfo {
                    meta: true,
                    version: "test".into(),
                },
                authenticated: true,
                compatibility: ControllerCompatibility::Compatible,
                generation: 7,
            },
            observed_at_ms: 1,
        },
        capture: CaptureStatus {
            system_proxy: Observation::Fresh {
                value: SystemProxySessionSnapshot {
                    intent_enabled: false,
                    actual: SystemProxyStatus::default(),
                    ownership: SystemProxyOwnershipState::Unowned,
                },
                observed_at_ms: 1,
            },
            tun: Observation::Fresh {
                value: TunCaptureStatus {
                    requested: false,
                    configured: false,
                    permission: CapabilityState::Unknown,
                    runtime: crate::TunRuntimeObserver::observe(&crate::TunConfig::default()),
                    observed: CapabilityState::Inactive,
                },
                observed_at_ms: 1,
            },
        },
        ..OperationalSnapshot::default()
    }
}

fn spawn_test_controller_server(
    listener: std::net::TcpListener,
) -> (
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    use std::io::Write;
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_flag.load(std::sync::atomic::Ordering::Acquire) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let Ok(request) = crate::test_support::read_controller_request_blocking(&mut stream)
            else {
                continue;
            };
            let bytes = request.into_bytes();
            let req = String::from_utf8_lossy(&bytes);
            let body = if req.starts_with("GET /version ") {
                r#"{"version":"test"}"#
            } else if req.starts_with("GET /connections ") {
                r#"{"connections":[],"memory":47000000,"uploadTotal":0,"downloadTotal":0}"#
            } else if req.starts_with("GET /configs ") {
                r#"{"mode":"rule","tun":{"enable":false}}"#
            } else {
                "{}"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (stop, handle)
}

#[test]
fn failure_preserves_last_successful_value_and_timestamp_as_stale() {
    let previous = Observation::Fresh {
        value: 42,
        observed_at_ms: 10,
    };

    let observed = Observation::record(&previous, Err("offline"), 20, RecoveryAction::Retry);

    assert_eq!(
        observed,
        Observation::Stale {
            value: 42,
            observed_at_ms: 10,
            failure: OperationalFailure {
                message: "offline".into(),
                occurred_at_ms: 20,
            },
        }
    );
}

#[test]
fn first_failure_has_no_invented_value_and_exposes_recovery() {
    let observed = Observation::<u8>::record(
        &Observation::Loading,
        Err("unauthorized"),
        20,
        RecoveryAction::InspectCore,
    );

    assert!(matches!(
        observed,
        Observation::Failed {
            recovery: RecoveryAction::InspectCore,
            ..
        }
    ));
    assert_eq!(observed.value(), None);
}

#[test]
fn independently_loaded_failure_can_merge_with_a_previous_success() {
    let previous = Observation::Fresh {
        value: "profile-a",
        observed_at_ms: 10,
    };
    let next = Observation::Failed {
        failure: OperationalFailure {
            message: "controller offline".into(),
            occurred_at_ms: 20,
        },
        recovery: RecoveryAction::Retry,
    };

    assert!(matches!(
        Observation::retain_last_success(&previous, next),
        Observation::Stale {
            value: "profile-a",
            observed_at_ms: 10,
            ..
        }
    ));
}

#[test]
fn traffic_activity_does_not_imply_capture() {
    let capture = CaptureStatus {
        system_proxy: Observation::Fresh {
            value: SystemProxySessionSnapshot {
                intent_enabled: false,
                actual: SystemProxyStatus::default(),
                ownership: SystemProxyOwnershipState::Unowned,
            },
            observed_at_ms: 1,
        },
        tun: Observation::Fresh {
            value: TunCaptureStatus {
                requested: false,
                configured: false,
                permission: CapabilityState::Unknown,
                runtime: crate::TunRuntimeObserver::observe(&crate::TunConfig::default()),
                observed: CapabilityState::Inactive,
            },
            observed_at_ms: 1,
        },
    };
    let traffic = Observation::Fresh {
        value: StreamStatus {
            generation: 1,
            last_success_at_ms: 1,
            upload: 1_024,
            download: 2_048,
            ..StreamStatus::default()
        },
        observed_at_ms: 1,
    };

    assert!(traffic.is_fresh());
    assert!(!capture.is_active());
}

#[test]
fn configured_tun_is_unknown_until_runtime_activation_is_observed() {
    let config = RuntimeConfig {
        tun: crate::TunConfig {
            enable: true,
            ..crate::TunConfig::default()
        },
        ..RuntimeConfig::default()
    };

    let status = TunCaptureStatus::from_config(CoreKind::Mihomo, &config);

    assert!(status.requested);
    assert!(status.configured);
    assert_eq!(status.observed, CapabilityState::Unknown);
}

#[test]
fn configured_tun_requires_permission_device_and_route_evidence() {
    let runtime = crate::TunRuntimeObservation {
        device_name: Some("Mihomo".into()),
        device: CapabilityState::Active,
        route: CapabilityState::Inactive,
        detail: "route uses another interface".into(),
    };

    assert_eq!(
        aggregate_tun_state(true, CapabilityState::Active, &runtime),
        CapabilityState::Inactive
    );
    assert_eq!(
        aggregate_tun_state(true, CapabilityState::Unknown, &runtime),
        CapabilityState::Inactive
    );
    let active_runtime = crate::TunRuntimeObservation {
        route: CapabilityState::Active,
        ..runtime
    };
    assert_eq!(
        aggregate_tun_state(true, CapabilityState::Active, &active_runtime),
        CapabilityState::Active
    );
}

#[test]
fn async_result_from_an_old_generation_is_rejected() {
    let expected = CoreSessionSnapshot {
        kind: CoreKind::Mihomo,
        managed: true,
        running: Some(true),
        generation: 3,
    };
    let current = CoreSessionSnapshot {
        generation: 4,
        ..expected
    };

    assert!(!same_generation(expected, current));
    let mut streams = StreamStatuses {
        memory: Observation::Fresh {
            value: StreamStatus {
                generation: 3,
                memory: 47_000_000,
                ..Default::default()
            },
            observed_at_ms: 1,
        },
        ..Default::default()
    };
    reset_old_generation_streams(&mut streams, current.generation);
    assert_eq!(streams.memory, Observation::Loading);
}

#[test]
fn generation_reset_preserves_current_evidence_and_unversioned_failures() {
    let current = Observation::Fresh {
        value: StreamStatus {
            generation: 2,
            ..Default::default()
        },
        observed_at_ms: 10,
    };
    let failed = Observation::Failed {
        failure: OperationalFailure {
            message: "disconnected".into(),
            occurred_at_ms: 11,
        },
        recovery: RecoveryAction::Retry,
    };
    let mut streams = StreamStatuses {
        memory: Observation::Fresh {
            value: StreamStatus {
                generation: 1,
                ..Default::default()
            },
            observed_at_ms: 5,
        },
        traffic: current.clone(),
        logs: Observation::Stale {
            value: StreamStatus {
                generation: 1,
                ..Default::default()
            },
            observed_at_ms: 5,
            failure: OperationalFailure {
                message: "old stream".into(),
                occurred_at_ms: 6,
            },
        },
        connections: failed.clone(),
    };
    reset_old_generation_streams(&mut streams, 2);
    assert_eq!(streams.memory, Observation::Loading);
    assert_eq!(streams.logs, Observation::Loading);
    assert_eq!(streams.traffic, current);
    assert_eq!(streams.connections, failed);
}

#[test]
fn uptime_requires_a_live_managed_process_and_a_valid_clock() {
    let snapshot = controllable_snapshot();
    let mut process = snapshot.process.value().unwrap().clone();
    process.started_at_secs = Some(100);
    assert_eq!(process.uptime_secs_at(100), Some(0));
    assert_eq!(process.uptime_secs_at(3_761), Some(3_661));
    assert_eq!(process.uptime_secs_at(99), None);
    process.running = false;
    assert_eq!(process.uptime_secs_at(200), None);
    process.running = true;
    process.managed = false;
    assert_eq!(process.uptime_secs_at(200), None);
    process.managed = true;
    process.started_at_secs = Some(0);
    assert_eq!(process.uptime_secs_at(200), None);
    process.started_at_secs = None;
    assert_eq!(process.uptime_secs_at(200), None);
}
