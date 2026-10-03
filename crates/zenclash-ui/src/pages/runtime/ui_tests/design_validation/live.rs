//! Opt-in real subscription validation; all generated configuration stays in a temporary directory.

use super::*;
use zenclash_core::{
    ConnectionPolicy, MihomoEndpoint, ProxyCatalog, ProxyGroupBehavior, ProxyOperations,
};

pub(super) struct LiveValidation {
    pub(super) catalog: ProxyCatalog,
    pub(super) mode: String,
    pub(super) pages: Vec<(Page, RuntimeData)>,
    _connections: Vec<std::net::TcpStream>,
}

pub(super) fn prepare(source: PathBuf) -> (Fixture, LiveValidation, Arc<MihomoProcess>) {
    let mut fixture = Fixture::new();
    let binary = PathBuf::from(
        std::env::var_os("ZENCLASH_MIHOMO_BINARY").expect("set ZENCLASH_MIHOMO_BINARY"),
    );
    let original: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&source).unwrap())
        .unwrap_or_else(|_| panic!("subscription YAML could not be parsed"));
    assert!(original.is_mapping(), "subscription must be a YAML mapping");
    let mut isolated = serde_yaml::Mapping::new();
    for key in [
        "proxies",
        "proxy-groups",
        "rules",
        "proxy-providers",
        "rule-providers",
        "geox-url",
        "geodata-mode",
        "geodata-loader",
    ] {
        if let Some(value) = original.get(key) {
            isolated.insert(key.into(), value.clone());
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    isolated.insert("mixed-port".into(), port.into());
    isolated.insert("allow-lan".into(), false.into());
    isolated.insert("bind-address".into(), "127.0.0.1".into());
    isolated.insert("mode".into(), "rule".into());
    isolated.insert("log-level".into(), "info".into());
    let controller = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint =
        MihomoEndpoint::with_random_secret(controller.local_addr().unwrap().to_string()).unwrap();
    let generated = fixture.root.join("isolated.yaml");
    fs::write(&generated, serde_yaml::to_string(&isolated).unwrap()).unwrap();
    let home = fixture.root.join("core");
    fs::create_dir_all(&home).unwrap();
    if let Some(data) = std::env::var_os("ZENCLASH_MIHOMO_GEODATA") {
        fs::copy(data, home.join("geoip.metadb")).unwrap();
    }
    for key in ["proxy-providers", "rule-providers"] {
        if let Some(providers) = isolated
            .get_mut(serde_yaml::Value::from(key))
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            for (index, (_, provider)) in providers.iter_mut().enumerate() {
                let target = home.join(format!("{key}-{index}.yaml"));
                if provider.get("type").and_then(serde_yaml::Value::as_str) == Some("file") {
                    let path = provider
                        .get("path")
                        .and_then(serde_yaml::Value::as_str)
                        .expect("file provider needs path");
                    fs::copy(source.parent().unwrap().join(path), &target)
                        .expect("copy file provider into isolated home");
                }
                provider
                    .as_mapping_mut()
                    .expect("provider mapping")
                    .insert("path".into(), target.to_string_lossy().to_string().into());
            }
        }
    }
    fs::write(&generated, serde_yaml::to_string(&isolated).unwrap()).unwrap();
    let previous = fixture.profiles.load().unwrap().active.unwrap();
    let record = if let Some(url_file) = std::env::var_os("ZENCLASH_UI_VALIDATION_URL_FILE") {
        let url = fs::read_to_string(url_file).unwrap();
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .block_on(
                fixture
                    .profiles
                    .add_remote("Live subscription", url.trim(), "clash.meta"),
            )
            .unwrap_or_else(|_| panic!("real subscription import failed"))
    } else {
        fixture.profiles.import_local(&generated).unwrap()
    };
    fixture.profile = fixture.profiles.activate(&record.id).unwrap();
    fixture.profiles.delete(&previous).unwrap();
    drop(listener);
    drop(controller);
    let process = MihomoProcess::spawn(
        MihomoLaunchConfig::new(binary, &generated, home)
            .unwrap()
            .with_controller_endpoint(endpoint.clone()),
    )
    .unwrap();
    fixture.managed_process = Some(process.clone());
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(process.wait_until_ready(Duration::from_secs(60)))
        .unwrap_or_else(|_| panic!("isolated Mihomo did not become ready"));
    let client = MihomoClient::from_process(process.clone()).unwrap();
    fixture.status.stop();
    fixture.core = CoreSession::open_with_config(
        CoreKind::Mihomo,
        client.clone(),
        Some(fixture.profile.clone()),
        Vec::new(),
    )
    .unwrap();
    fixture.traffic = TrafficMonitor::start(runtime.handle(), endpoint.clone());
    fixture.logs = LogMonitor::start(runtime.handle(), endpoint, MihomoLogLevel::Info);
    fixture.status = OperationalStatus::start(
        runtime.handle(),
        fixture.core.clone(),
        None,
        fixture.traffic.clone(),
        fixture.logs.clone(),
    );
    let live = runtime.block_on(async {
        let version = client.version().await.unwrap();
        let mut catalog = client.proxy_catalog().await.unwrap();
        let mut nodes = physical_nodes(&catalog);
        for _ in 0..60 {
            if !nodes.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            catalog = client.proxy_catalog().await.unwrap();
            nodes = physical_nodes(&catalog);
        }
        assert!(!nodes.is_empty(), "provider did not load physical nodes");
        let candidates = (0..nodes.len().min(6))
            .map(|index| nodes[index * nodes.len() / nodes.len().min(6)].clone())
            .collect::<Vec<_>>();
        let delays = futures_util::future::join_all(candidates.iter().map(|(name, provider)| {
            client.proxy_delay_with_provider(name, None, 8_000, provider.as_deref())
        }))
        .await;
        let successful = candidates
            .iter()
            .zip(&delays)
            .filter_map(|(name, result)| {
                result
                    .as_ref()
                    .ok()
                    .map(|delay| (name.0.clone(), delay.delay))
            })
            .collect::<Vec<_>>();
        assert!(
            !successful.is_empty(),
            "no subscription node passed the real delay check"
        );
        let catalog = client.proxy_catalog().await.unwrap();
        assert!(
            !catalog.groups().is_empty(),
            "subscription has no strategy groups"
        );
        let selection = catalog.groups().iter().find_map(|group| {
            if group.behavior != ProxyGroupBehavior::Selector || group.name == "GLOBAL" {
                return None;
            }
            successful
                .iter()
                .find(|(name, _)| {
                    name != &group.now
                        && group
                            .all
                            .iter()
                            .any(|id| catalog.node(id).is_some_and(|node| &node.name == name))
                })
                .map(|(name, _)| (group.name.clone(), name.clone()))
        });
        let switched = if let Some((group, name)) = selection {
            let outcome = ProxyOperations::new(client.clone())
                .select(&group, &name, ConnectionPolicy::KeepExisting)
                .await
                .unwrap();
            assert_eq!(outcome.actual.as_deref(), Some(name.as_str()));
            true
        } else {
            false
        };
        let (connections, http_status) =
            tokio::task::spawn_blocking(move || open_proxy_connections(port))
                .await
                .unwrap();
        assert!(switched, "no different selectable node was confirmed");
        assert_eq!(
            http_status,
            Some(204),
            "real HTTP proxy request did not succeed"
        );
        assert!(
            !connections.is_empty(),
            "no proxy CONNECT session established"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        let live_connections = client.connections_snapshot().await.unwrap();
        assert!(
            connections.iter().any(|socket| {
                let port = socket.local_addr().unwrap().port().to_string();
                live_connections
                    .connections
                    .iter()
                    .any(|connection| connection.metadata.source_port == port)
            }),
            "held proxy session missing from the real controller snapshot"
        );
        let mut pages = Vec::new();
        for page in [
            Page::Home,
            Page::Profiles,
            Page::Connections,
            Page::Rules,
            Page::Logs,
            Page::Settings,
        ] {
            pages.push((
                page,
                loader::load_page(client.clone(), page)
                    .await
                    .unwrap_or_else(|_| panic!("live page load failed")),
            ));
        }
        let catalog = client.proxy_catalog().await.unwrap();
        let rules = client.rule_catalog().await.unwrap();
        let report = serde_json::json!({
            "core_version": version.version,
            "physical_node_count": nodes.len(),
            "proxy_count": catalog.proxy_count,
            "group_count": catalog.groups().len(),
            "rule_count": rules.rules.len(),
            "delay_attempts": candidates.len(),
            "delay_successes": successful.len(),
            "delay_ms": successful.iter().map(|(_, delay)| *delay).collect::<Vec<_>>(),
            "selection_readback_verified": switched,
            "proxy_http_status": http_status,
            "held_proxy_connections": connections.len(),
            "connection_snapshot_count": live_connections.connections.len(),
            "system_proxy_modified": false,
            "tun_enabled": false,
        });
        let output = PathBuf::from(
            std::env::var_os("ZENCLASH_UI_VALIDATION_OUTPUT")
                .expect("set private output directory"),
        );
        fs::create_dir_all(&output).unwrap();
        fs::write(
            output.join("live-report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!("Live validation: {report}");
        LiveValidation {
            catalog,
            mode: "rule".into(),
            pages,
            _connections: connections,
        }
    });
    (fixture, live, process)
}

fn physical_nodes(catalog: &ProxyCatalog) -> Vec<(String, Option<String>)> {
    let groups = catalog
        .groups()
        .iter()
        .map(|group| group.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    catalog
        .groups()
        .iter()
        .flat_map(|group| group.all.iter())
        .filter_map(|id| {
            let node = catalog.node(id)?;
            if groups.contains(node.name.as_str())
                || matches!(
                    node.kind.as_str(),
                    "Direct" | "Reject" | "Compatible" | "Pass"
                )
                || !seen.insert(id.clone())
            {
                return None;
            }
            Some((node.name.clone(), id.provider().map(str::to_owned)))
        })
        .collect()
}

fn open_proxy_connections(port: u16) -> (Vec<std::net::TcpStream>, Option<u16>) {
    use std::io::{Read, Write};
    let request = |authority: &str,
                   connect: bool|
     -> std::io::Result<(std::net::TcpStream, Option<u16>)> {
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", port))?;
        socket.set_read_timeout(Some(Duration::from_secs(8)))?;
        socket.set_write_timeout(Some(Duration::from_secs(8)))?;
        let text = if connect {
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n")
        } else {
            "GET http://www.gstatic.com/generate_204 HTTP/1.1\r\nHost: www.gstatic.com\r\nConnection: close\r\n\r\n".into()
        };
        socket.write_all(text.as_bytes())?;
        let mut response = [0; 1024];
        let count = socket.read(&mut response)?;
        let status = std::str::from_utf8(&response[..count])
            .ok()
            .and_then(|response| response.split_whitespace().nth(1)?.parse().ok());
        Ok((socket, status))
    };
    let status = request("www.gstatic.com", false)
        .ok()
        .and_then(|(_, status)| status);
    let connections = [
        "github.com:443",
        "www.cloudflare.com:443",
        "www.gstatic.com:443",
    ]
    .into_iter()
    .filter_map(|authority| {
        request(authority, true)
            .ok()
            .filter(|(_, status)| *status == Some(200))
            .map(|(socket, _)| socket)
    })
    .collect();
    (connections, status)
}
