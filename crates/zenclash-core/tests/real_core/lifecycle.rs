use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use zenclash_core::{
    CapabilityState, ControlledConfigStore, CoreKind, CoreLaunchConfig, CoreSession, LogMonitor,
    LogStreamFormat, LogTimeSource, ManagedCoreProcess, MihomoClient, MihomoEndpoint,
    MihomoLogLevel, NetworkLatencyTarget, NetworkProbeRoute, NetworkProbeService, Observation,
    OperationalStatus, TrafficMonitor,
};

use crate::{
    config_and_overrides::{
        verify_controlled_mode_switch, verify_profile_workflows,
        verify_real_authorized_subscription, verify_real_ruleset_conversion, verify_runtime_api,
    },
    harness::{
        IntegrationInputs, reserve_loopback_port, start_mihomo, wait_for_real_mixed_listener,
    },
    proxies_and_rules::{
        verify_catalog_apis, verify_real_direct_proxy_delay, verify_real_proxy_operations,
    },
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn managed_ipc_carries_http_and_all_realtime_streams_across_restart() {
    use futures_util::StreamExt as _;
    let _serial = crate::real_core_lock().await;
    let binary =
        PathBuf::from(std::env::var_os("ZENCLASH_MIHOMO_BINARY").expect("real kernel binary"));
    let root = std::env::temp_dir().join(format!("zenclash-real-ipc-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("profile.yaml");
    fs::write(&profile, "mixed-port: 0\nexternal-controller: ''\nmode: rule\nlog-level: debug\ndns:\n  enable: false\nproxies: []\nproxy-groups:\n  - name: test group\n    type: select\n    proxies: [DIRECT, REJECT]\nrules: ['MATCH,DIRECT']\n").unwrap();
    let launch = CoreLaunchConfig::new(binary, &profile, root.join("home")).unwrap();
    assert!(launch.endpoint.ipc_path().is_some());
    assert!(launch.controller_override.is_none());
    #[cfg(feature = "test-support")]
    let process = ManagedCoreProcess::spawn_isolated_for_test(launch).unwrap();
    #[cfg(not(feature = "test-support"))]
    let process = ManagedCoreProcess::spawn(launch).unwrap();
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap_or_else(|error| panic!("{error}\n{}", process.snapshot().logs.join("\n")));
    let client = MihomoClient::from_process(process.clone()).unwrap();
    let traffic =
        TrafficMonitor::start_with_client(&tokio::runtime::Handle::current(), client.clone());
    assert!(client.version().await.unwrap().meta);
    for _ in 0..4 {
        let requests = (0..32).map(|_| client.version());
        for result in futures_util::future::join_all(requests).await {
            result.expect("concurrent IPC requests must wait for an available pipe instance");
        }
    }
    assert!(!client.proxy_catalog().await.unwrap().groups().is_empty());
    client.change_proxy("test group", "REJECT").await.unwrap();
    assert_eq!(
        client.proxy_group_selection("test group").await.unwrap(),
        "REJECT"
    );
    client.memory_snapshot().await.unwrap();
    client.connections_snapshot().await.unwrap();
    let controller = zenclash_core::service::NativeController::new_local(
        process.endpoint().ipc_path().unwrap().to_path_buf(),
        process.snapshot().pid.unwrap(),
        process.endpoint().secret.clone(),
    );
    for path in ["/traffic", "/memory", "/connections"] {
        let mut stream = controller.websocket(path).await.unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(frame.is_text(), "{path} must produce a JSON frame");
        let _: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    }
    let mut logs = controller.websocket("/logs?level=debug").await.unwrap();
    let reload = controller
        .request(
            "PUT",
            "/configs?force=true",
            Some(&serde_json::json!({"payload": fs::read_to_string(&profile).unwrap()})),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(reload.status, 204);
    let log = tokio::time::timeout(Duration::from_secs(5), logs.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(log.is_text());
    let _: serde_json::Value = serde_json::from_str(log.to_text().unwrap()).unwrap();
    drop(logs);
    let old_pid = process.snapshot().pid.unwrap();
    let previous_traffic_frame = traffic.snapshot().updated_at_ms;
    process.restart().unwrap();
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap();
    assert_ne!(process.snapshot().pid.unwrap(), old_pid);
    assert!(client.version().await.unwrap().meta);
    client.memory_snapshot().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = traffic.snapshot();
            if snapshot.connected && snapshot.updated_at_ms > previous_traffic_frame {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("GPUI's shared traffic client must reconnect to the restarted IPC kernel");
    drop(traffic);
    process.stop().unwrap();
    assert!(client.version().await.is_err());
    drop(client);
    drop(process);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fs::remove_dir_all(root).unwrap();
}

/// This test intentionally has no mock server. Set `ZENCLASH_MIHOMO_BINARY` to
/// an actual Mihomo executable before running it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn drives_the_supplied_profile_through_a_real_mihomo_process() {
    let _serial = crate::real_core_lock().await;
    let inputs = IntegrationInputs::from_env();
    inputs.seed_real_geodata();
    verify_real_ruleset_conversion(&inputs);
    verify_real_listener_conflict_fallback(&inputs).await;
    let process = start_mihomo(&inputs).await;
    let client = MihomoClient::from_process(process.clone())
        .expect("real client")
        .with_config_validator(process.config_validator())
        .unwrap();
    verify_real_operational_status(&client, process.clone()).await;
    let persistent_logs = LogMonitor::start(
        &tokio::runtime::Handle::current(),
        process.endpoint().clone(),
        MihomoLogLevel::Debug,
    );
    let persistent_log_path = inputs.home.join("integration-continuous-mihomo.log");
    persistent_logs
        .configure_persistence(&persistent_log_path, true, 1)
        .expect("configure production persistent log writer");

    verify_real_direct_proxy_delay(&client).await;
    verify_real_proxy_operations(&client, &inputs.profile, &inputs.home).await;
    verify_persistent_log_stream(
        &client,
        &persistent_logs,
        &persistent_log_path,
        &inputs.profile,
    )
    .await;
    verify_runtime_api(&client, &inputs.profile, &inputs.home).await;
    verify_controlled_mode_switch(&client, &inputs.profile, &inputs.home).await;
    verify_profile_workflows(&client, process.clone(), &inputs.profile, &inputs.home).await;
    verify_catalog_apis(&client).await;
    verify_traffic_stream(&process).await;
    verify_managed_restart(&process, &client, &inputs.profile, &inputs.home).await;

    process.stop().expect("stop real Mihomo");
}

async fn verify_real_operational_status(client: &MihomoClient, process: Arc<ManagedCoreProcess>) {
    let runtime = tokio::runtime::Handle::current();
    let traffic = TrafficMonitor::start(&runtime, process.endpoint().clone());
    let logs = LogMonitor::start(&runtime, process.endpoint().clone(), MihomoLogLevel::Info);
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    let status = OperationalStatus::start(&runtime, session, None, traffic, logs);

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = status.snapshot();
            if snapshot.controller.is_fresh()
                && snapshot.streams.traffic.is_fresh()
                && snapshot.capture.tun.is_fresh()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real operational status did not become fresh");

    let snapshot = status.snapshot();
    assert!(matches!(
        snapshot.capture.system_proxy,
        Observation::Failed { .. }
    ));
    assert_eq!(
        snapshot.capture.tun.value().map(|tun| tun.observed),
        Some(CapabilityState::Inactive)
    );
    assert!(snapshot.streams.traffic.is_fresh());
    assert!(!snapshot.capture.is_active());
}

async fn verify_real_listener_conflict_fallback(inputs: &IntegrationInputs) {
    let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve a conflicting real proxy port");
    let original_port = occupied.local_addr().unwrap().port();
    let controller_reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve listener fallback controller");
    let controller = controller_reservation.local_addr().unwrap();
    let root = inputs.home.join("listener-fallback-integration");
    std::fs::create_dir_all(&root).expect("create listener fallback integration directory");
    let profile = root.join("source.yaml");
    let source = format!(
        "mixed-port: {original_port}\nallow-lan: false\nmode: rule\nexternal-controller: {controller}\nproxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n"
    );
    std::fs::write(&profile, &source).expect("write listener fallback source profile");
    let controlled = ControlledConfigStore::new(root.join("controlled"));
    let runtime_profile = controlled
        .materialize(&profile)
        .expect("materialize listener fallback profile");
    let fallbacks = controlled
        .resolve_startup_listener_conflicts()
        .expect("resolve occupied listener with a session fallback");
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0].original, original_port);
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), source);

    let endpoint = MihomoEndpoint::with_random_secret(controller.to_string())
        .expect("generate listener fallback controller secret");
    drop(controller_reservation);
    let launch = CoreLaunchConfig::new(&inputs.binary, runtime_profile, root.join("home"))
        .expect("build listener fallback launch")
        .with_controller_endpoint(endpoint);
    launch
        .validate_config()
        .expect("real Mihomo validates the fallback runtime profile");
    let process = ManagedCoreProcess::spawn(launch).expect("start real Mihomo on fallback port");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .expect("real Mihomo becomes ready on fallback port");
    let client = MihomoClient::new(process.endpoint().clone()).expect("listener fallback client");
    let actual = wait_for_real_mixed_listener(&process, &client, fallbacks[0].current).await;
    assert_eq!(actual.mixed_port, fallbacks[0].current);
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, actual.mixed_port)),
        Duration::from_secs(5),
    )
    .expect("real fallback proxy listener accepts connections");
    process.stop().expect("stop listener fallback Mihomo");
}

async fn verify_persistent_log_stream(
    client: &MihomoClient,
    monitor: &LogMonitor,
    log_path: &Path,
    profile: &Path,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !monitor.connected() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real /logs WebSocket did not connect");

    let probe_port = reserve_loopback_port();
    client
        .patch_configs_verified(&serde_json::json!({
            "mixed-port": probe_port,
            "log-level": "debug"
        }))
        .await
        .expect("enable a real local proxy port for log verification");
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, probe_port))
        .expect("connect to real Mihomo mixed proxy");
    stream
        .write_all(
            b"GET http://127.0.0.1:1/ HTTP/1.1\r\nHost: 127.0.0.1:1\r\nConnection: close\r\n\r\n",
        )
        .expect("send real proxied request");
    verify_real_mihomo_network_probe(probe_port).await;
    verify_real_authorized_subscription(client, probe_port, log_path.parent().unwrap()).await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = monitor.persistence_status();
            if status.size_bytes > 0 && log_path.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real Mihomo logs were not persisted");

    let status = monitor.persistence_status();
    assert!(status.enabled);
    assert!(status.last_error.is_none(), "log writer error: {status:?}");
    let content = std::fs::read_to_string(log_path).expect("read persisted real Mihomo log");
    assert!(content.contains("INFO") || content.contains("DEBUG"));
    assert_eq!(
        monitor.stream_snapshot().format,
        LogStreamFormat::Structured,
        "real Mihomo should honor format=structured"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if monitor.entries().iter().any(|entry| {
                entry.time_source == LogTimeSource::Core
                    && entry.core_time.is_some()
                    && !entry.fields.is_null()
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real structured log entry did not arrive");
    client
        .reload_config(profile, true)
        .await
        .expect("restore supplied profile after persistent-log probe");
}

async fn verify_real_mihomo_network_probe(proxy_port: u16) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("start real loopback HTTP origin");
    let origin = listener
        .local_addr()
        .expect("read real HTTP origin address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request proxied by Mihomo");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set origin read timeout");
        let mut request = [0_u8; 4096];
        let length = stream
            .read(&mut request)
            .expect("read proxied HTTP request");
        assert!(
            String::from_utf8_lossy(&request[..length]).contains("GET"),
            "Mihomo did not forward an HTTP request"
        );
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("write real HTTP origin response");
    });
    let probe = NetworkProbeService::new(NetworkProbeRoute::MihomoHttp {
        host: "127.0.0.1".into(),
        port: proxy_port,
    })
    .expect("construct production Mihomo network probe");
    let result = probe
        .measure_target(
            NetworkLatencyTarget::new("real-mihomo-loopback", format!("http://{origin}/probe"))
                .expect("valid real probe target"),
        )
        .await;

    assert!(
        result.error.is_none(),
        "production network probe did not traverse real Mihomo: {:?}",
        result.error
    );
    assert!(result.latency_ms.is_some());
    server.join().expect("real HTTP origin thread completed");
}

async fn verify_traffic_stream(process: &ManagedCoreProcess) {
    let traffic = TrafficMonitor::start(
        &tokio::runtime::Handle::current(),
        process.endpoint().clone(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !traffic.snapshot().connected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("real /traffic WebSocket did not connect");
}

async fn verify_managed_restart(
    process: &ManagedCoreProcess,
    client: &MihomoClient,
    profile: &Path,
    home: &Path,
) {
    let controlled = ControlledConfigStore::new(home.join("controlled-config-integration"));
    controlled
        .apply_json_update(
            client,
            profile,
            &serde_json::json!({"mode": "global", "mixed-port": 17891}),
        )
        .await
        .expect("persist restart-sensitive controlled settings");
    assert!(
        std::fs::read_to_string(controlled.runtime_path())
            .expect("read managed startup cache")
            .contains("mixed-port: 17891"),
        "controlled update did not synchronize the managed startup cache"
    );
    let first_pid = process.snapshot().pid.expect("running Mihomo PID");
    process.restart().expect("restart real Mihomo process");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .expect("restarted real Mihomo becomes ready");
    let second_pid = process.snapshot().pid.expect("restarted Mihomo PID");
    assert_ne!(first_pid, second_pid, "restart must replace the child PID");
    let restarted = wait_for_real_mixed_listener(process, client, 17_891).await;
    assert_eq!(restarted.mode.to_ascii_lowercase(), "global");
    assert_eq!(restarted.mixed_port, 17_891);
}
