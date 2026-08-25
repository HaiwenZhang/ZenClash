use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use zenclash_core::{
    MihomoClient, MihomoLaunchConfig, MihomoProcess, ProfileStore, TrafficMonitor,
};

/// This test intentionally has no mock server. Set `ZENCLASH_MIHOMO_BINARY` to
/// an actual Mihomo executable before running it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn drives_the_supplied_profile_through_a_real_mihomo_process() {
    let inputs = IntegrationInputs::from_env();
    inputs.seed_real_geodata();
    let process = start_mihomo(&inputs).await;
    let client = MihomoClient::new(process.endpoint().clone()).expect("real client");

    verify_runtime_api(&client, &inputs.profile).await;
    verify_profile_workflows(&client, &inputs.profile, &inputs.home).await;
    verify_catalog_apis(&client).await;
    verify_traffic_stream(&process).await;

    process.stop().expect("stop real Mihomo");
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
            || workspace.join("examples/19facdf022b.yaml"),
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
    let launch = MihomoLaunchConfig::new(&inputs.binary, &inputs.profile, inputs.home.clone())
        .expect("real launch config")
        .with_controller_override(&inputs.controller);
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

async fn verify_runtime_api(client: &MihomoClient, profile: &Path) {
    let version = client.version().await.expect("GET /version");
    assert!(version.meta, "expected the Mihomo/Clash.Meta core");
    assert!(!version.version.is_empty());

    let config = client.runtime_config().await.expect("GET /configs");
    assert_eq!(config.mode.to_ascii_lowercase(), "rule");
    assert!(config.ipv6);
    client
        .reload_config(&profile, true)
        .await
        .expect("PUT /configs hot reload of the supplied profile");
}

async fn verify_profile_workflows(client: &MihomoClient, profile: &Path, home: &Path) {
    let profile_store = ProfileStore::new(home.join("profile-integration-store"))
        .expect("create integration profile store");
    let local = profile_store
        .import_local(profile)
        .expect("import the real local Clash YAML");
    let local_path = profile_store
        .activate(&local.id)
        .expect("persist local profile as active");
    client
        .reload_config(&local_path, true)
        .await
        .expect("real Mihomo accepts the managed local profile");

    let (subscription_url, subscription_server) = serve_subscription(profile);
    let remote = profile_store
        .add_remote("真实在线订阅", subscription_url, "ZenClash-Integration")
        .await
        .expect("download real Clash YAML through subscription workflow");
    subscription_server
        .join()
        .expect("subscription server completed");
    let remote_path = profile_store.profile_path(&remote);
    client
        .reload_config(&remote_path, true)
        .await
        .expect("real Mihomo accepts the downloaded subscription");
    profile_store
        .activate(&remote.id)
        .expect("persist remote subscription as active");
    assert_eq!(
        profile_store.active_path().expect("read active profile"),
        Some(remote_path)
    );
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
    assert!(catalog.proxy_count >= 50, "unexpected profile proxy count");
    assert!(catalog.groups.len() >= 10, "unexpected profile group count");
    let group = catalog.groups.first().expect("at least one proxy group");
    assert!(!group.now.is_empty());
    client
        .change_proxy(&group.name, &group.now)
        .await
        .expect("PUT /proxies/:group");

    let rules = client.rule_catalog().await.expect("GET /rules");
    assert!(
        rules.rules.len() >= 9_000,
        "expected the supplied full rule set"
    );
    let providers = client
        .proxy_provider_catalog()
        .await
        .expect("GET /providers/proxies");
    assert!(providers.providers.len() >= 10);

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

fn workspace_path(workspace: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}
