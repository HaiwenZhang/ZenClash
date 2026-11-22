#[cfg(feature = "test-support")]
use std::path::PathBuf;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    sync::Arc,
    thread,
    time::Duration,
};

#[cfg(feature = "test-support")]
use zenclash_core::CoreLaunchConfig;
use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreSession, DnsRecordType, ManagedCoreProcess, MihomoClient,
    ProfileApplication, ProfileApplyOutcome, ProfileChange, ProfileStore, RemoteProfileOptions,
    RulesetBehavior, RulesetConverter, SystemNetworkSnapshot, YamlOverrideStore,
};

use crate::harness::{IntegrationInputs, serve_subscription, start_test_dns_server};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "test-support")]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn runtime_config_accepts_real_mihomo_null_tun_dns_hijack() {
    let binary =
        PathBuf::from(std::env::var_os("ZENCLASH_MIHOMO_BINARY").expect("real kernel binary"));
    let temp_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    };
    let root = temp_root.join(format!(
        "zc-null-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let profile = root.join("profile.yaml");
    fs::write(&profile, "mixed-port: 0\nexternal-controller: ''\nmode: rule\ntun: {enable: false, dns-hijack: null}\ndns: {enable: false}\nrules: ['MATCH,DIRECT']\n").unwrap();
    let process = ManagedCoreProcess::spawn_isolated_for_test(
        CoreLaunchConfig::new(binary, &profile, root.join("home")).unwrap(),
    )
    .unwrap();
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap();
    let controller = zenclash_core::service::NativeController::new_local(
        process.endpoint().ipc_path().unwrap().to_path_buf(),
        process.snapshot().pid.unwrap(),
        process.endpoint().secret.clone(),
    );
    let response = controller
        .request("GET", "/configs", None, Duration::from_secs(5))
        .await
        .unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(raw["tun"]["dns-hijack"], serde_json::Value::Null);

    let client = MihomoClient::from_process(process.clone()).unwrap();
    let config = client
        .runtime_config()
        .await
        .expect("nullable /configs response must decode");
    assert!(config.tun.dns_hijack.is_empty());
    assert!(!config.tun.enable);
    assert_eq!(config.mode, "rule");
    process.stop().unwrap();
    drop(client);
    drop(process);
    fs::remove_dir_all(root).unwrap();
}

pub(crate) fn verify_real_ruleset_conversion(inputs: &IntegrationInputs) {
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

pub(crate) async fn verify_controlled_mode_switch(
    client: &MihomoClient,
    profile: &Path,
    home: &Path,
) {
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

pub(crate) async fn verify_real_authorized_subscription(
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

pub(crate) async fn verify_runtime_api(client: &MihomoClient, profile: &Path, home: &Path) {
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

pub(crate) async fn verify_profile_workflows(
    client: &MihomoClient,
    process: Arc<ManagedCoreProcess>,
    profile: &Path,
    home: &Path,
) {
    let profile_store = ProfileStore::new(home.join("profile-integration-store"))
        .expect("create integration profile store");
    let controlled = ControlledConfigStore::new(home.join("profile-integration-controlled"));
    assert_eq!(client.endpoint(), Some(process.endpoint().clone()));
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
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
