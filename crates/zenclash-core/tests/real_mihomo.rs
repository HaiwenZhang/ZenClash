use std::{path::PathBuf, time::Duration};

use zenclash_core::{MihomoClient, MihomoLaunchConfig, MihomoProcess, TrafficMonitor};

/// This test intentionally has no mock server. Set `ZENCLASH_MIHOMO_BINARY` to
/// an actual Mihomo executable before running it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn drives_the_supplied_profile_through_a_real_mihomo_process() {
    let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
        .map(PathBuf::from)
        .expect("set ZENCLASH_MIHOMO_BINARY to a real Mihomo executable");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root");
    let profile = workspace.join("examples/19facdf022b.yaml");
    assert!(profile.is_file(), "missing profile: {}", profile.display());

    let controller = std::env::var("ZENCLASH_INTEGRATION_CONTROLLER")
        .unwrap_or_else(|_| "127.0.0.1:19091".to_owned());
    let home = std::env::var_os("ZENCLASH_INTEGRATION_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("zenclash-real-mihomo-{}", std::process::id()))
        });
    let launch = MihomoLaunchConfig::new(binary, &profile, home)
        .expect("real launch config")
        .with_controller_override(controller);
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

    let client = MihomoClient::new(process.endpoint().clone()).expect("real client");
    let version = client.version().await.expect("GET /version");
    assert!(version.meta, "expected the Mihomo/Clash.Meta core");
    assert!(!version.version.is_empty());

    let config = client.runtime_config().await.expect("GET /configs");
    assert_eq!(config.mode.to_ascii_lowercase(), "rule");
    assert!(config.ipv6);
    client
        .reload_config(&profile.to_string_lossy(), true)
        .await
        .expect("PUT /configs hot reload of the supplied profile");

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

    process.stop().expect("stop real Mihomo");
}
