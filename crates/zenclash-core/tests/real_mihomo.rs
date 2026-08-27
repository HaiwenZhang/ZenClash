use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::Duration,
};

use zenclash_core::{
    CapabilityState, ConnectionPolicy, ControlledConfigStore, CoreKind, CoreSession, DnsRecordType,
    LogMonitor, LogStreamFormat, LogTimeSource, MihomoClient, MihomoEndpoint, MihomoLaunchConfig,
    MihomoLogLevel, MihomoProcess, NetworkLatencyTarget, NetworkProbeRoute, NetworkProbeService,
    Observation, OperationalStatus, ProfileApplication, ProfileApplyOutcome, ProfileChange,
    ProfileStore, ProxyGroupBehavior, ProxyOperations, ProxyVisibility, RemoteProfileOptions,
    RulesetBehavior, RulesetConverter, SystemNetworkSnapshot, TrafficMonitor, YamlOverrideStore,
};

/// This test intentionally has no mock server. Set `ZENCLASH_MIHOMO_BINARY` to
/// an actual Mihomo executable before running it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn drives_the_supplied_profile_through_a_real_mihomo_process() {
    let inputs = IntegrationInputs::from_env();
    inputs.seed_real_geodata();
    verify_real_ruleset_conversion(&inputs);
    verify_real_listener_conflict_fallback(&inputs).await;
    let process = start_mihomo(&inputs).await;
    let client = MihomoClient::new(process.endpoint().clone())
        .expect("real client")
        .with_config_validator(process.config_validator());
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

async fn verify_real_operational_status(client: &MihomoClient, process: Arc<MihomoProcess>) {
    let runtime = tokio::runtime::Handle::current();
    let traffic = TrafficMonitor::start(&runtime, process.endpoint().clone());
    let logs = LogMonitor::start(&runtime, process.endpoint().clone(), MihomoLogLevel::Info);
    let session = CoreSession::open(CoreKind::Mihomo, client.clone(), Some(process));
    let status = OperationalStatus::start(&runtime, session, None, None, traffic, logs);

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
    let launch = MihomoLaunchConfig::new(&inputs.binary, runtime_profile, root.join("home"))
        .expect("build listener fallback launch")
        .with_controller_endpoint(endpoint);
    launch
        .validate_config()
        .expect("real Mihomo validates the fallback runtime profile");
    let process = MihomoProcess::spawn(launch).expect("start real Mihomo on fallback port");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .expect("real Mihomo becomes ready on fallback port");
    let actual = MihomoClient::new(process.endpoint().clone())
        .expect("listener fallback client")
        .runtime_config()
        .await
        .expect("read listener fallback runtime config");
    assert_eq!(actual.mixed_port, fallbacks[0].current);
    process.stop().expect("stop listener fallback Mihomo");
}

async fn verify_controlled_mode_switch(client: &MihomoClient, profile: &Path, home: &Path) {
    let controlled = ControlledConfigStore::new(home.join("controlled-mode-integration"));
    for mode in ["global", "direct", "rule"] {
        controlled
            .apply_mode_update_with_overrides(client, profile, mode, Vec::new())
            .await
            .unwrap_or_else(|error| panic!("switch real Mihomo to {mode}: {error}"));
        assert_eq!(
            client
                .runtime_config()
                .await
                .expect("read real Mihomo mode after switch")
                .mode
                .to_ascii_lowercase(),
            mode
        );
    }
    assert_eq!(
        controlled
            .load_json()
            .expect("read persisted real Mihomo mode")
            .get("mode")
            .and_then(serde_json::Value::as_str),
        Some("rule")
    );
}

async fn verify_real_direct_proxy_delay(client: &MihomoClient) {
    let result = client
        .proxy_delay(
            "DIRECT",
            Some("https://www.gstatic.com/generate_204"),
            5_000,
        )
        .await
        .expect("measure DIRECT through the real Mihomo delay API");

    assert!(result.delay > 0, "real DIRECT delay must be positive");
}

async fn verify_real_proxy_operations(client: &MihomoClient, profile: &Path, home: &Path) {
    fs::create_dir_all(home).expect("create real proxy-operation integration home");
    let operation_profile = home.join("proxy-operations-integration.yaml");
    fs::write(
        &operation_profile,
        concat!(
            "mode: rule\n",
            "log-level: info\n",
            "proxies: []\n",
            "proxy-groups:\n",
            "  - name: Selector Test\n",
            "    type: select\n",
            "    proxies: [DIRECT]\n",
            "  - name: Automatic Test\n",
            "    type: url-test\n",
            "    proxies: [DIRECT]\n",
            "    url: https://www.gstatic.com/generate_204\n",
            "    interval: 300\n",
            "rules:\n",
            "  - MATCH,Selector Test\n",
        ),
    )
    .expect("write real proxy-operation profile");
    client
        .reload_config(&operation_profile, true)
        .await
        .expect("load real proxy-operation profile");

    let operations = ProxyOperations::new(client.clone());
    let initial_connections = client
        .connections_snapshot()
        .await
        .expect("read connections before real selection");
    let selected = operations
        .select("Selector Test", "DIRECT", ConnectionPolicy::KeepExisting)
        .await
        .expect("select through real Mihomo without rebuilding connections");
    assert_eq!(selected.actual.as_deref(), Some("DIRECT"));
    assert_eq!(
        client
            .connections_snapshot()
            .await
            .expect("read connections after real selection")
            .connections,
        initial_connections.connections
    );

    let fixed = operations
        .select("Automatic Test", "DIRECT", ConnectionPolicy::KeepExisting)
        .await
        .expect("fix real automatic group");
    let fixed_group = fixed
        .catalog
        .as_ref()
        .and_then(|catalog| {
            catalog
                .groups
                .iter()
                .find(|group| group.name == "Automatic Test")
        })
        .expect("read back real automatic group after selection");
    assert_eq!(
        fixed_group.behavior,
        ProxyGroupBehavior::Automatic { fixed: true }
    );

    let restored = operations
        .restore_auto("Automatic Test")
        .await
        .expect("restore real automatic group");
    let restored_group = restored
        .catalog
        .as_ref()
        .and_then(|catalog| {
            catalog
                .groups
                .iter()
                .find(|group| group.name == "Automatic Test")
        })
        .expect("read back real automatic group after restore");
    assert_eq!(
        restored_group.behavior,
        ProxyGroupBehavior::Automatic { fixed: false }
    );
    assert!(
        operations
            .catalog(ProxyVisibility::VisibleOnly)
            .await
            .expect("read real visible proxy catalog")
            .groups
            .iter()
            .all(|group| !group.hidden)
    );

    client
        .reload_config(profile, true)
        .await
        .expect("restore supplied profile after real proxy-operation verification");
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

async fn verify_real_authorized_subscription(
    client: &MihomoClient,
    proxy_port: u16,
    integration_home: &Path,
) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("start real subscription HTTP origin");
    let origin = listener
        .local_addr()
        .expect("read subscription origin address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("accept subscription request forwarded by Mihomo");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set subscription origin timeout");
        let mut request = [0_u8; 8_192];
        let length = stream
            .read(&mut request)
            .expect("read subscription origin request");
        let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
        assert!(request.contains("authorization: bearer real-mihomo-secret\r\n"));
        assert!(request.contains("user-agent: zenclash-real-integration\r\n"));
        let payload = format!(
            "mixed-port: {proxy_port}\nmode: rule\nproxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n"
        );
        write!(
            stream,
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/yaml\r\n",
                "Subscription-Userinfo: upload=100; download=200; total=10000; expire=2000000000\r\n",
                "Profile-Web-Page-Url: https://example.com/real-account\r\n",
                "Profile-Update-Interval: 12\r\n",
                "Content-Length: {}\r\n",
                "Connection: close\r\n\r\n{}"
            ),
            payload.len(),
            payload
        )
        .expect("write real subscription response");
    });
    let store = ProfileStore::new(integration_home.join("real-subscription-store"))
        .expect("create real subscription store");
    let options =
        RemoteProfileOptions::new("Bearer real-mihomo-secret", true).expect("valid auth options");
    let record = store
        .add_remote_with_options(
            "real-authorized-subscription",
            format!("http://{origin}/profile.yaml"),
            "ZenClash-Real-Integration",
            options,
            Some(proxy_port),
        )
        .await
        .expect("download authorized subscription through real Mihomo");
    server.join().expect("real subscription origin completed");

    assert_eq!(record.subscription.usage.as_ref().unwrap().used(), 300);
    assert_eq!(record.update_interval_minutes, 12 * 60);
    client
        .reload_config(&store.profile_path(&record), true)
        .await
        .expect("real Mihomo accepts downloaded subscription YAML");

    let profile_path = store.profile_path(&record);
    let original = fs::read_to_string(&profile_path).expect("read editable real profile");
    let candidate = original.replacen("mode: rule", "mode: direct", 1);
    assert_ne!(
        candidate, original,
        "real profile must contain editable mode"
    );
    let update = store
        .replace_payload(&record.id, &original, &candidate)
        .expect("atomically replace real profile payload");
    client
        .reload_config(&profile_path, true)
        .await
        .expect("real Mihomo accepts edited profile YAML");
    store
        .rollback_update(update)
        .expect("rollback real profile edit");
    client
        .reload_config(&profile_path, true)
        .await
        .expect("real Mihomo accepts restored profile YAML");
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

fn reserve_loopback_port() -> u16 {
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve loopback port")
        .local_addr()
        .expect("read reserved loopback port")
        .port()
}

fn verify_real_ruleset_conversion(inputs: &IntegrationInputs) {
    std::fs::create_dir_all(&inputs.home).expect("create real Mihomo integration home");
    let source = inputs.home.join("integration-domain-rules.txt");
    let binary = inputs.home.join("integration-domain-rules.mrs");
    std::fs::write(&source, "+.example.com\nfull.example.org\n")
        .expect("write real ruleset source");
    let output = Command::new(&inputs.binary)
        .args([
            "convert-ruleset",
            "domain",
            "text",
            source.to_str().expect("UTF-8 integration source path"),
            binary.to_str().expect("UTF-8 integration binary path"),
        ])
        .output()
        .expect("run real Mihomo ruleset encoder");
    assert!(
        output.status.success(),
        "real Mihomo ruleset encoding failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let conversion = RulesetConverter::new(&inputs.binary)
        .convert_mrs_to_text(&binary, RulesetBehavior::Domain)
        .expect("decode real MRS through production converter");

    assert_eq!(conversion.content, "+.example.com\nfull.example.org\n");
    assert_eq!(conversion.behavior, RulesetBehavior::Domain);
    assert!(conversion.source_bytes > 0);
}

struct IntegrationInputs {
    binary: PathBuf,
    profile: PathBuf,
    home: PathBuf,
    controller: String,
}

impl IntegrationInputs {
    fn from_env() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
            .map(PathBuf::from)
            .map(|path| workspace_path(workspace, path))
            .expect("set ZENCLASH_MIHOMO_BINARY to a real Mihomo executable");
        let profile = std::env::var_os("ZENCLASH_CONFIG").map_or_else(
            || workspace.join("platforms/common/default.yaml"),
            |path| workspace_path(workspace, PathBuf::from(path)),
        );
        assert!(profile.is_file(), "missing profile: {}", profile.display());
        let home = std::env::var_os("ZENCLASH_INTEGRATION_HOME").map_or_else(
            || std::env::temp_dir().join(format!("zenclash-real-mihomo-{}", std::process::id())),
            PathBuf::from,
        );
        let controller = std::env::var("ZENCLASH_INTEGRATION_CONTROLLER")
            .unwrap_or_else(|_| "127.0.0.1:19091".to_owned());
        Self {
            binary,
            profile,
            home,
            controller,
        }
    }

    fn seed_real_geodata(&self) {
        let Some(source) = std::env::var_os("ZENCLASH_INTEGRATION_GEODATA_DIR").map(PathBuf::from)
        else {
            return;
        };
        std::fs::create_dir_all(&self.home).expect("create real Mihomo integration home");
        let mut copied = 0;
        for name in ["geoip.metadb", "country.mmdb", "geoip.dat", "geosite.dat"] {
            let source_file = source.join(name);
            if source_file.is_file() {
                std::fs::copy(&source_file, self.home.join(name))
                    .unwrap_or_else(|error| panic!("copy {}: {error}", source_file.display()));
                copied += 1;
            }
        }
        assert!(
            copied > 0,
            "no real GeoData files found in {}",
            source.display()
        );
    }
}

async fn start_mihomo(inputs: &IntegrationInputs) -> Arc<MihomoProcess> {
    let controlled = ControlledConfigStore::new(inputs.home.join("controlled-config-integration"));
    let runtime_profile = controlled
        .materialize(&inputs.profile)
        .expect("materialize managed startup profile");
    let endpoint = MihomoEndpoint::with_random_secret(&inputs.controller)
        .expect("generate authenticated integration controller endpoint");
    let launch = MihomoLaunchConfig::new(&inputs.binary, runtime_profile, inputs.home.clone())
        .expect("real launch config")
        .with_controller_endpoint(endpoint);
    let process = MihomoProcess::spawn(launch).expect("start real Mihomo");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "real Mihomo did not become ready: {error}\n{}",
                process.snapshot().logs.join("\n")
            )
        });
    process
}

async fn verify_runtime_api(client: &MihomoClient, profile: &Path, home: &Path) {
    let version = client.version().await.expect("GET /version");
    assert!(version.meta, "expected the Mihomo/Clash.Meta core");
    assert!(!version.version.is_empty());

    let config = client.runtime_config().await.expect("GET /configs");
    assert_eq!(config.mode.to_ascii_lowercase(), "rule");
    assert!(config.ipv6);
    client
        .patch_configs_verified(&serde_json::json!({"ipv6": false}))
        .await
        .expect("PATCH /configs runtime setting");
    assert!(
        !client
            .runtime_config()
            .await
            .expect("GET /configs after runtime patch")
            .ipv6,
        "Mihomo did not apply the runtime patch"
    );
    let unsupported_sniffer_patch = client
        .patch_configs_verified(&serde_json::json!({
            "sniffer": {
                "enable": true,
                "force-dns-mapping": true,
                "parse-pure-ip": true,
                "override-destination": true
            }
        }))
        .await;
    assert!(
        unsupported_sniffer_patch.is_err(),
        "verified patch must reject a field that Mihomo silently ignores"
    );
    verify_controlled_advanced_config(client, profile, home).await;
    client
        .reload_config(&profile, true)
        .await
        .expect("PUT /configs hot reload of the supplied profile");
    assert!(
        client
            .runtime_config()
            .await
            .expect("GET /configs after profile restore")
            .ipv6,
        "profile reload did not restore the supplied real configuration"
    );
}

async fn verify_controlled_advanced_config(client: &MihomoClient, profile: &Path, home: &Path) {
    let controlled = ControlledConfigStore::new(home.join("controlled-config-integration"));
    let system_interface = SystemNetworkSnapshot::detect().interface;
    let (dns_server, dns_thread) = start_test_dns_server();
    let update = advanced_config_update(&system_interface, &dns_server);
    controlled
        .apply_json_update(client, profile, &update)
        .await
        .expect("controlled YAML reload through real Mihomo");
    let runtime = client
        .runtime_config()
        .await
        .expect("GET /configs after controlled listener reload");
    assert!(
        runtime.sniffing.enable,
        "full YAML reload did not apply controlled sniffer settings"
    );
    assert_eq!(runtime.mixed_port, 17_890);
    assert_eq!(runtime.bind_address, "127.0.0.1");
    assert_eq!(runtime.log_level.to_ascii_lowercase(), "warning");
    if !system_interface.is_empty() {
        assert_eq!(runtime.interface_name, system_interface);
    }
    let dns_a = client
        .dns_query("diagnostics.zenclash.test", DnsRecordType::A)
        .await
        .expect("query a deterministic A record through real Mihomo DNS");
    assert_eq!(dns_a.status, 0);
    assert!(
        dns_a
            .answer
            .iter()
            .any(|answer| answer.record_type == 1 && answer.ttl > 0)
    );
    let dns_aaaa = client
        .dns_query("diagnostics.zenclash.test", DnsRecordType::Aaaa)
        .await
        .expect("query an independent AAAA record through real Mihomo DNS");
    assert_eq!(dns_aaaa.status, 0);
    client
        .flush_dns_cache()
        .await
        .expect("flush the real Mihomo DNS cache independently");
    client
        .flush_fake_ip_cache()
        .await
        .expect("flush the real Mihomo fake-IP cache independently");
    let queried_types = dns_thread.join().expect("local DNS server completed");
    assert!(
        queried_types.contains(&1),
        "real Mihomo did not send an A query"
    );
    assert!(
        queried_types.contains(&28),
        "real Mihomo did not send an AAAA query"
    );
    let persisted = controlled
        .load_json()
        .expect("load persisted advanced controlled config");
    assert_eq!(
        persisted
            .pointer("/dns/ipv6")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        persisted
            .pointer("/sniffer/sniff/HTTP/ports/1")
            .and_then(serde_json::Value::as_str),
        Some("8080-8880")
    );
    assert_eq!(
        persisted
            .pointer("/tun/route-exclude-address/0")
            .and_then(serde_json::Value::as_str),
        Some("192.0.2.0/24")
    );
    verify_explicit_override_preserves_controlled(client, profile, home, &controlled).await;
}

fn advanced_config_update(system_interface: &str, dns_server: &str) -> serde_json::Value {
    let mut update = serde_json::json!({
        "port": 0,
        "socks-port": 0,
        "mixed-port": 17890,
        "redir-port": 0,
        "tproxy-port": 0,
        "bind-address": "127.0.0.1",
        "log-level": "warning",
        "sniffer": {
            "enable": true,
            "force-dns-mapping": true,
            "parse-pure-ip": true,
            "override-destination": true,
            "sniff": {
                "HTTP": {"ports": [80, "8080-8880"]},
                "TLS": {"ports": [443, 8443]},
                "QUIC": {"ports": [443, 8443]}
            },
            "skip-domain": ["Mijia Cloud"],
            "force-domain": ["+.example.com"],
            "skip-dst-address": ["192.0.2.0/24"],
            "skip-src-address": ["198.51.100.0/24"]
        },
        "dns": {
            "enable": true,
            "ipv6": true,
            "enhanced-mode": "fake-ip",
            "fake-ip-range": "198.18.0.1/16",
            "fake-ip-filter-mode": "blacklist",
            "fake-ip-filter": ["*.lan"],
            "default-nameserver": ["223.5.5.5"],
            "nameserver": [dns_server],
            "proxy-server-nameserver": [dns_server],
            "direct-nameserver": ["223.5.5.5"],
            "fallback": [dns_server],
            "fallback-filter": {
                "geoip": false,
                "geoip-code": "CN",
                "ipcidr": ["240.0.0.0/4"],
                "domain": ["+.google.com"]
            },
            "nameserver-policy": {"+.example.com": "223.5.5.5"}
        },
        "hosts": {"zenclash.test": "192.0.2.1"},
        "tun": {
            "enable": false,
            "stack": "mixed",
            "device": "utun1500",
            "mtu": 1500,
            "auto-route": true,
            "auto-detect-interface": true,
            "strict-route": false,
            "dns-hijack": ["any:53", "tcp://any:53"],
            "route-address": ["0.0.0.0/1"],
            "route-exclude-address": ["192.0.2.0/24"]
        }
    });
    if !system_interface.is_empty() {
        update
            .as_object_mut()
            .expect("controlled integration update is an object")
            .insert(
                "interface-name".into(),
                serde_json::Value::String(system_interface.to_owned()),
            );
    }
    update
}

fn start_test_dns_server() -> (String, thread::JoinHandle<Vec<u16>>) {
    let socket =
        UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind deterministic DNS server");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set deterministic DNS timeout");
    let address = socket.local_addr().expect("deterministic DNS address");
    let server = thread::spawn(move || {
        let mut queried_types = Vec::new();
        let mut buffer = [0_u8; 2_048];
        loop {
            let Ok((length, peer)) = socket.recv_from(&mut buffer) else {
                break;
            };
            let Some((question_end, record_type)) = dns_question(&buffer[..length]) else {
                continue;
            };
            queried_types.push(record_type);
            let response = dns_response(&buffer[..length], question_end, record_type);
            socket
                .send_to(&response, peer)
                .expect("reply from deterministic DNS server");
        }
        queried_types
    });
    (format!("udp://{address}"), server)
}

fn dns_question(query: &[u8]) -> Option<(usize, u16)> {
    if query.len() < 17 {
        return None;
    }
    let mut cursor = 12;
    while *query.get(cursor)? != 0 {
        let label_len = usize::from(*query.get(cursor)?);
        cursor = cursor.checked_add(label_len + 1)?;
    }
    let question_end = cursor.checked_add(5)?;
    let record_type = u16::from_be_bytes([*query.get(cursor + 1)?, *query.get(cursor + 2)?]);
    (question_end <= query.len()).then_some((question_end, record_type))
}

fn dns_response(query: &[u8], question_end: usize, record_type: u16) -> Vec<u8> {
    let answer = match record_type {
        1 => Some(vec![192, 0, 2, 1]),
        28 => Some(
            std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)
                .octets()
                .to_vec(),
        ),
        _ => None,
    };
    let mut response =
        Vec::with_capacity(question_end + answer.as_ref().map_or(0, |data| data.len() + 12));
    response.extend_from_slice(&query[..2]);
    response.extend_from_slice(&[0x81, 0x80]);
    response.extend_from_slice(&[0, 1]);
    response.extend_from_slice(&[0, u8::from(answer.is_some())]);
    response.extend_from_slice(&[0, 0, 0, 0]);
    response.extend_from_slice(&query[12..question_end]);
    if let Some(answer) = answer {
        response.extend_from_slice(&[0xc0, 0x0c]);
        response.extend_from_slice(&record_type.to_be_bytes());
        response.extend_from_slice(&[0, 1]);
        response.extend_from_slice(&120_u32.to_be_bytes());
        response.extend_from_slice(&u16::try_from(answer.len()).unwrap().to_be_bytes());
        response.extend_from_slice(&answer);
    }
    response
}

async fn verify_explicit_override_preserves_controlled(
    client: &MihomoClient,
    profile: &Path,
    home: &Path,
    controlled: &ControlledConfigStore,
) {
    let source = home.join("integration-override-sources");
    std::fs::create_dir_all(&source).expect("create integration override source directory");
    std::fs::write(source.join("10-global.yaml"), "mode: global\n")
        .expect("write first integration override");
    std::fs::write(source.join("20-direct.yaml"), "mode: direct\n")
        .expect("write second integration override");
    let store = YamlOverrideStore::new(home.join("integration-yaml-overrides"))
        .expect("create persistent override store");
    let imported = store
        .import_paths([source])
        .expect("import ordered real Mihomo overrides");
    let mut catalog = store.load().expect("load persistent override catalog");
    controlled
        .reload_with_overrides(client, profile, store.enabled_paths(&catalog))
        .await
        .expect("reload controlled config plus explicit override");
    let overridden = client
        .runtime_config()
        .await
        .expect("GET /configs after explicit override");
    assert_eq!(overridden.mode.to_ascii_lowercase(), "direct");
    assert!(
        overridden.sniffing.enable,
        "explicit override reload discarded controlled sniffer settings"
    );

    let before = catalog.clone();
    catalog.items.swap(0, 1);
    store
        .replace_catalog(&before, &catalog)
        .expect("persist reordered override catalog");
    controlled
        .reload_with_overrides(client, profile, store.enabled_paths(&catalog))
        .await
        .expect("reload reordered persistent overrides");
    assert_eq!(
        client
            .runtime_config()
            .await
            .expect("GET /configs after override reorder")
            .mode
            .to_ascii_lowercase(),
        "global"
    );

    let before = catalog.clone();
    catalog
        .items
        .iter_mut()
        .find(|record| record.id == imported[0].id)
        .expect("find global override")
        .enabled = false;
    store
        .replace_catalog(&before, &catalog)
        .expect("persist disabled override");
    controlled
        .apply_json_update_with_overrides(
            client,
            profile,
            &serde_json::json!({"dns": {"ipv6": true}}),
            store.enabled_paths(&catalog),
        )
        .await
        .expect("settings update preserves persistent overrides");
    let updated = client
        .runtime_config()
        .await
        .expect("GET /configs after settings update with override");
    assert_eq!(updated.mode.to_ascii_lowercase(), "direct");
    assert!(updated.ipv6);
}

async fn verify_profile_workflows(
    client: &MihomoClient,
    process: Arc<MihomoProcess>,
    profile: &Path,
    home: &Path,
) {
    let profile_store = ProfileStore::new(home.join("profile-integration-store"))
        .expect("create integration profile store");
    let controlled = ControlledConfigStore::new(home.join("profile-integration-controlled"));
    let session = CoreSession::open(CoreKind::Mihomo, client.clone(), Some(process));
    let application = ProfileApplication::new(profile_store.clone(), controlled, session);
    let local = match application
        .apply(ProfileChange::ImportLocal {
            source: profile.to_path_buf(),
            overrides: Vec::new(),
        })
        .await
    {
        ProfileApplyOutcome::Applied { profile, .. } => profile,
        outcome => panic!("real ProfileApplication local import failed: {outcome:?}"),
    };

    let (subscription_url, subscription_server) = serve_subscription(profile);
    let remote = match application
        .apply(ProfileChange::AddRemote {
            name: "真实在线订阅".into(),
            url: subscription_url,
            user_agent: "ZenClash-Integration".into(),
            options: RemoteProfileOptions::default(),
            overrides: Vec::new(),
        })
        .await
    {
        ProfileApplyOutcome::Applied { profile, .. } => profile,
        outcome => panic!("real ProfileApplication remote add failed: {outcome:?}"),
    };
    subscription_server
        .join()
        .expect("subscription server completed");
    let remote_path = profile_store.profile_path(&remote);
    assert_eq!(
        profile_store.active_path().expect("read active profile"),
        Some(remote_path)
    );
    assert_ne!(local.id, remote.id);
}

fn serve_subscription(profile: &Path) -> (String, thread::JoinHandle<()>) {
    let subscription_payload = std::fs::read(profile).expect("read real subscription payload");
    let listener =
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind subscription endpoint");
    let subscription_url = format!(
        "http://{}/clash.yaml",
        listener.local_addr().expect("subscription address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept subscription request");
        let mut request = [0_u8; 8_192];
        let read = stream
            .read(&mut request)
            .expect("read subscription request");
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(
            request
                .to_ascii_lowercase()
                .contains("user-agent: zenclash-integration"),
            "custom subscription User-Agent was not sent: {request}"
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            subscription_payload.len()
        )
        .expect("write subscription headers");
        stream
            .write_all(&subscription_payload)
            .expect("write subscription payload");
    });
    (subscription_url, server)
}

async fn verify_catalog_apis(client: &MihomoClient) {
    let catalog = client.proxy_catalog().await.expect("GET /proxies");
    assert!(
        catalog.proxy_count > 0,
        "Mihomo did not expose built-in proxies"
    );
    if let Some(group) = catalog.groups.first() {
        assert!(!group.now.is_empty());
        client
            .change_proxy(&group.name, &group.now)
            .await
            .expect("PUT /proxies/:group");
    }

    let rules = client.rule_catalog().await.expect("GET /rules");
    assert!(
        !rules.rules.is_empty(),
        "Mihomo did not expose active rules"
    );
    let (mutable_rule_index, originally_disabled) = rules
        .rules
        .iter()
        .find_map(|rule| Some((rule.index?, rule.extra.as_ref()?.disabled)))
        .expect("real Mihomo exposes indexed mutable rule state");
    client
        .set_rule_disabled(mutable_rule_index, !originally_disabled)
        .await
        .expect("PATCH /rules/disable and verify changed state");
    client
        .set_rule_disabled(mutable_rule_index, originally_disabled)
        .await
        .expect("restore original rule disabled state");
    let providers = client
        .proxy_provider_catalog()
        .await
        .expect("GET /providers/proxies");
    assert!(
        providers
            .providers
            .values()
            .all(|provider| !provider.name.is_empty()),
        "provider identities must not be empty"
    );

    let connections = client
        .connections_snapshot()
        .await
        .expect("GET /connections");
    assert_eq!(connections.connections.len(), 0);
}

async fn verify_traffic_stream(process: &MihomoProcess) {
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
    process: &MihomoProcess,
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
    let restarted = client
        .runtime_config()
        .await
        .expect("GET /configs after restart");
    assert_eq!(restarted.mode.to_ascii_lowercase(), "global");
    assert_eq!(restarted.mixed_port, 17_891);
}

fn workspace_path(workspace: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}
