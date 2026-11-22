use super::*;

#[tokio::test]
async fn zero_memory_stream_uses_the_same_samples_connection_memory() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint =
        crate::MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(
                !request.starts_with("GET /configs "),
                "fast tick without refresh_platform must not fetch /configs"
            );
            assert!(
                !request.starts_with("GET /memory "),
                "fast tick should reuse connections.memory instead of opening /memory"
            );
            let body = if request.starts_with("GET /version ") {
                r#"{"version":"test"}"#
            } else {
                assert!(request.starts_with("GET /connections "));
                r#"{"connections":[],"memory":47000000,"uploadTotal":0,"downloadTotal":0}"#
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let core = CoreSession::open(
        CoreKind::Mihomo,
        crate::MihomoClient::new(endpoint).unwrap(),
    )
    .unwrap();
    let runtime = Handle::current();
    let offline = crate::MihomoEndpoint::new("http://127.0.0.1:1", "");
    let traffic = TrafficMonitor::start(&runtime, offline.clone());
    let logs = LogMonitor::start(&runtime, offline, crate::MihomoLogLevel::Info);
    let (snapshot, _) = watch::channel(OperationalSnapshot::default());
    let status = Arc::new(OperationalStatus {
        snapshot,
        task: std::sync::OnceLock::new(),
        wake: Arc::new(tokio::sync::Notify::new()),
        core_session: None,
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(refresh_status_with_tun_reader(
            &status,
            &core,
            None,
            &traffic,
            &logs,
            false,
            |kind, config| async move { Ok(TunCaptureStatus::from_config(kind, &config)) },
        )),
    )
    .await
    .unwrap();
    let snapshot = status.snapshot();
    assert!(
        snapshot.streams.memory.is_fresh(),
        "a zero memory frame hid the connection endpoint's memory"
    );
    assert_eq!(snapshot.streams.memory.value().unwrap().memory, 47_000_000);
    assert_eq!(
        snapshot.streams.memory.value().unwrap().generation,
        core.generation()
    );
    server.join().unwrap();
}

#[tokio::test]
async fn a_transition_during_the_final_platform_read_rejects_the_old_sample() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint =
        crate::MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let server = std::thread::spawn(move || {
        for index in 0..6 {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let length = request.len();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = if request.starts_with("GET /version ") {
                r#"{"version":"test"}"#
            } else if request.starts_with("GET /connections ") {
                r#"{"connections":[],"uploadTotal":0,"downloadTotal":0}"#
            } else if index < 4 {
                r#"{"mode":"rule"}"#
            } else {
                r#"{"mode":"global"}"#
            };
            if request.starts_with("PATCH ") {
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
            } else {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        }
    });
    let client = crate::MihomoClient::new(endpoint).unwrap();
    let core = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let runtime = Handle::current();
    let offline = crate::MihomoEndpoint::new("http://127.0.0.1:1", "");
    let traffic = TrafficMonitor::start(&runtime, offline.clone());
    let logs = LogMonitor::start(&runtime, offline, crate::MihomoLogLevel::Info);
    let (snapshot, _) = watch::channel(OperationalSnapshot::default());
    let status = Arc::new(OperationalStatus {
        snapshot,
        task: std::sync::OnceLock::new(),
        wake: Arc::new(tokio::sync::Notify::new()),
        core_session: None,
    });
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let sampling = {
        let status = status.clone();
        let core = core.clone();
        let traffic = traffic.clone();
        let logs = logs.clone();
        tokio::spawn(async move {
            Box::pin(refresh_status_with_tun_reader(
                &status,
                &core,
                None,
                &traffic,
                &logs,
                true,
                |kind, config| async move {
                    entered.send(()).unwrap();
                    released.await.unwrap();
                    Ok(TunCaptureStatus::from_config(kind, &config))
                },
            ))
            .await;
        })
    };
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    core.set_mode(
        &crate::ControlledConfigStore::new(std::env::temp_dir()),
        "global",
    )
    .await
    .unwrap();
    release.send(()).unwrap();
    sampling.await.unwrap();
    let snapshot = status.snapshot();
    assert_eq!(snapshot.process.value().unwrap().generation, 1);
    assert!(snapshot.controller.value().is_none());
    assert!(snapshot.capture.tun.value().is_none());
    assert!(snapshot.streams.connections.value().is_none());
    assert!(snapshot.streams.memory.value().is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn a_transition_between_polls_discards_tun_when_the_next_read_is_failed_or_skipped() {
    use std::io::Write;

    for refresh_platform in [false, true] {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            crate::MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let expected_requests = if refresh_platform { 4 } else { 3 };
        let server = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let request = crate::test_support::read_controller_request_blocking(&mut stream)
                    .unwrap()
                    .into_bytes();
                let length = request.len();
                let request = String::from_utf8_lossy(&request[..length]);
                let body = if request.starts_with("GET /version ") {
                    r#"{"version":"test"}"#
                } else if request.starts_with("GET /connections ") {
                    r#"{"connections":[],"uploadTotal":0,"downloadTotal":0}"#
                } else if request.starts_with("GET /configs ") {
                    r#"{"mode":"rule","tun":{"enable":true}}"#
                } else {
                    assert!(request.starts_with("GET /memory "));
                    "{}"
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let core = CoreSession::open(
            CoreKind::Mihomo,
            crate::MihomoClient::new(endpoint).unwrap(),
        )
        .unwrap();
        let mut previous = controllable_snapshot();
        if let Observation::Fresh { value, .. } = &mut previous.process {
            value.generation = core.generation();
        }
        if let Observation::Fresh { value, .. } = &mut previous.controller {
            value.generation = core.generation();
        }
        if let Observation::Fresh { value, .. } = &mut previous.capture.tun {
            value.requested = true;
            value.configured = true;
            value.observed = CapabilityState::Active;
        }
        previous.path = Observation::Fresh {
            value: PathStatus {
                route: ObservedPathRoute::Mihomo,
                target: "old-probe".into(),
                generation: core.generation(),
            },
            observed_at_ms: 1,
        };
        let (snapshot, _) = watch::channel(previous);
        let status = OperationalStatus {
            snapshot,
            task: std::sync::OnceLock::new(),
            wake: Arc::new(tokio::sync::Notify::new()),
            core_session: None,
        };
        core.invalidate_runtime_observations();
        let runtime = Handle::current();
        let offline = crate::MihomoEndpoint::new("http://127.0.0.1:1", "");
        let traffic = TrafficMonitor::start(&runtime, offline.clone());
        let logs = LogMonitor::start(&runtime, offline, crate::MihomoLogLevel::Info);
        tokio::time::timeout(
            Duration::from_secs(5),
            Box::pin(refresh_status_with_tun_reader(
                &status,
                &core,
                None,
                &traffic,
                &logs,
                refresh_platform,
                |_, _| async { Err("new TUN observation unavailable".into()) },
            )),
        )
        .await
        .unwrap();
        let current = status.snapshot();
        assert!(current.capture.tun.value().is_none());
        assert!(!current.capture.is_active());
        assert_eq!(current.path, Observation::Loading);
        assert_eq!(
            current.controller.fresh_value().unwrap().generation,
            core.generation()
        );
        assert_ne!(current.first_run_stage(true), FirstRunStage::Ready);
        server.join().unwrap();
    }
}

#[tokio::test]
async fn sampling_can_be_cancelled_without_waiting_for_a_controller_timeout() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint =
        crate::MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let runtime = tokio::runtime::Handle::current();
    let client = crate::MihomoClient::new(endpoint.clone()).unwrap();
    let core = crate::CoreSession::open(crate::CoreKind::Mihomo, client).unwrap();
    let traffic = crate::TrafficMonitor::start(&runtime, endpoint.clone());
    let logs = crate::LogMonitor::start(&runtime, endpoint, crate::MihomoLogLevel::Info);
    let traffic_weak = std::sync::Arc::downgrade(&traffic);
    let logs_weak = std::sync::Arc::downgrade(&logs);
    let status = OperationalStatus::start(&runtime, core, None, traffic, logs);
    tokio::task::yield_now().await;
    status.stop();
    drop(status);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while traffic_weak.upgrade().is_some() || logs_weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closing a target must promptly release its streams even when the HTTP peer stalls");
}

#[test]
fn status_refresh_schedule_probes_platform_on_the_first_tick() {
    let mut schedule = StatusRefreshSchedule::default();

    assert!(schedule.next_refreshes_platform());
}

#[test]
fn status_refresh_schedule_limits_platform_probes_to_thirty_seconds() {
    let mut schedule = StatusRefreshSchedule::default();
    let probes = (0..12)
        .filter(|_| schedule.next_refreshes_platform())
        .collect::<Vec<_>>();

    assert_eq!(probes, vec![0, 6]);
}

#[test]
fn status_refresh_schedule_can_request_immediate_platform_refresh() {
    let mut schedule = StatusRefreshSchedule::default();
    assert!(schedule.next_refreshes_platform());
    assert!(!schedule.next_refreshes_platform());
    schedule.request_platform_refresh();
    assert!(schedule.next_refreshes_platform());
}

#[tokio::test]
async fn dropping_operational_status_without_stop_aborts_monitor_and_releases_resources() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint =
        crate::MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let (stop_server, server) = spawn_test_controller_server(listener);
    let runtime = Handle::current();
    let client = crate::MihomoClient::new(endpoint.clone()).unwrap();
    let core = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let traffic = TrafficMonitor::start(&runtime, endpoint.clone());
    let logs = LogMonitor::start(&runtime, endpoint, crate::MihomoLogLevel::Info);
    let traffic_weak = Arc::downgrade(&traffic);
    let logs_weak = Arc::downgrade(&logs);
    let status = OperationalStatus::start(&runtime, core, None, traffic, logs);
    let mut updates = status.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !updates.borrow().controller.is_fresh() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    drop(updates);
    // Drop without calling `status.stop()` — Drop must abort the sleeping monitor task promptly.
    drop(status);
    tokio::time::timeout(Duration::from_secs(1), async {
        while traffic_weak.upgrade().is_some() || logs_weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping OperationalStatus without stop() must promptly release owned monitors");
    stop_server.store(true, std::sync::atomic::Ordering::Release);
    server.join().unwrap();
}

#[tokio::test]
async fn binding_switch_wakes_platform_refresh_immediately_without_30s_tun_loading_stall() {
    let first_listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let first_endpoint = crate::MihomoEndpoint::new(
        format!("http://{}", first_listener.local_addr().unwrap()),
        "",
    );
    let (stop_first, first_server) = spawn_test_controller_server(first_listener);

    let second_listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let second_endpoint = crate::MihomoEndpoint::new(
        format!("http://{}", second_listener.local_addr().unwrap()),
        "",
    );
    let (stop_second, second_server) = spawn_test_controller_server(second_listener);

    let runtime = Handle::current();
    let client = crate::MihomoClient::new(first_endpoint.clone()).unwrap();
    let core = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    let traffic = TrafficMonitor::start(&runtime, first_endpoint.clone());
    let logs = LogMonitor::start(&runtime, first_endpoint, crate::MihomoLogLevel::Info);
    let status = OperationalStatus::start(&runtime, core.clone(), None, traffic, logs);
    let mut updates = status.subscribe();

    // Wait for the initial platform probe (tick 0) to complete; without immediate wake-up,
    // the next platform probe would normally not happen for 30 seconds.
    tokio::time::timeout(Duration::from_secs(3), async {
        while !updates.borrow().capture.tun.is_fresh() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let next_generation = core.switch_to_direct(second_endpoint).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = updates.borrow().clone();
            if snapshot
                .process
                .fresh_value()
                .is_some_and(|process| process.generation == next_generation)
                && snapshot.capture.tun.is_fresh()
            {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("TUN observation must refresh promptly after binding/generation switch");

    drop(status);
    stop_first.store(true, std::sync::atomic::Ordering::Release);
    stop_second.store(true, std::sync::atomic::Ordering::Release);
    first_server.join().unwrap();
    second_server.join().unwrap();
}
