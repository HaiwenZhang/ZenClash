use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use zenclash_core::{
    ConnectionPolicy, CoreLaunchConfig, ManagedCoreProcess, MihomoClient, MihomoEndpoint,
    ProxyGroupBehavior, ProxyOperations, ProxyVisibility,
};

use crate::harness::wait_for_real_proxy_groups;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MIHOMO_BINARY pointing to a real Mihomo executable"]
async fn proxy_catalog_preserves_subscription_group_and_member_order_across_reload() {
    let binary =
        PathBuf::from(std::env::var_os("ZENCLASH_MIHOMO_BINARY").expect("real kernel binary"));
    // macOS's per-user temp directory plus the socket filename can exceed the
    // Unix socket path limit. Keep this isolated fixture under a short root.
    let temp_root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    };
    let root = temp_root.join(format!(
        "zc-order-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    // Loopback-only dummy nodes; no DNS, TUN, system proxy or outbound requests.
    let prefix = "mixed-port: 0\nexternal-controller: ''\nmode: rule\ndns: {enable: false}\nproxies:\n  - {name: z-node, type: http, server: 127.0.0.1, port: 9}\n  - {name: a-node, type: http, server: 127.0.0.1, port: 9}\nrules: ['MATCH,DIRECT']\nproxy-groups:\n";
    let definitions = [
        "  - {name: z-stream, type: select, proxies: [z-node, a-node, DIRECT]}\n",
        "  - {name: a-backup, type: select, proxies: [a-node, z-node]}\n",
        "  - {name: Proxy, type: select, proxies: [a-backup, z-stream, z-node]}\n",
    ];
    let payload = format!("{prefix}{}", definitions.concat());
    let profile = root.join("profile.yaml");
    fs::write(&profile, &payload).unwrap();
    let launch = CoreLaunchConfig::new(binary, &profile, root.join("home")).unwrap();
    #[cfg(feature = "test-support")]
    let process = ManagedCoreProcess::spawn_isolated_for_test(launch).unwrap();
    #[cfg(not(feature = "test-support"))]
    let process = ManagedCoreProcess::spawn(launch).unwrap();
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap_or_else(|error| panic!("{error}\n{}", process.snapshot().logs.join("\n")));
    let client = MihomoClient::from_process(process.clone()).unwrap();
    wait_for_real_proxy_groups(&client, &["z-stream", "a-backup", "Proxy"]).await;
    for expected in [
        ["z-stream", "a-backup", "Proxy"],
        ["Proxy", "z-stream", "a-backup"],
    ] {
        if expected[0] == "Proxy" {
            client
                .reload_payload(
                    format!(
                        "{prefix}{}{}{}",
                        definitions[2], definitions[0], definitions[1]
                    ),
                    true,
                )
                .await
                .unwrap();
        }
        let catalog = client.proxy_catalog().await.unwrap();
        assert_eq!(
            catalog
                .groups_for_mode("rule")
                .map(|group| group.name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        let group = catalog
            .groups()
            .iter()
            .find(|group| group.name == "z-stream")
            .unwrap();
        assert_eq!(
            group
                .all
                .iter()
                .map(|id| id.controller_name())
                .collect::<Vec<_>>(),
            ["z-node", "a-node", "DIRECT"]
        );
    }
    process.stop().unwrap();
    drop(client);
    drop(process);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn proxy_group_readiness_waits_for_initial_configuration_without_waiting_for_order() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        for (path, body) in [
            ("/version", r#"{"version":"starting","meta":true}"#),
            (
                "/proxies",
                r#"{"proxies":{"DIRECT":{"type":"Direct"},"GLOBAL":{"type":"Selector","all":["DIRECT"]}}}"#,
            ),
            (
                "/proxies",
                r#"{"proxies":{
                "GLOBAL":{"type":"Selector","all":["DIRECT","Proxy","z-stream","a-backup"]},
                "Proxy":{"type":"Selector","all":["DIRECT"]},
                "z-stream":{"type":"Selector","all":["DIRECT"]},
                "a-backup":{"type":"Selector","all":["DIRECT"]}
            }}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(format!("GET {path} ").as_bytes()));
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });

    client.version().await.unwrap();
    let catalog = wait_for_real_proxy_groups(&client, &["z-stream", "a-backup", "Proxy"]).await;
    assert_eq!(
        catalog
            .groups_for_mode("rule")
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>(),
        ["Proxy", "z-stream", "a-backup"]
    );
    server.await.unwrap();
}

pub(crate) async fn verify_real_direct_proxy_delay(client: &MihomoClient) {
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

pub(crate) async fn verify_real_proxy_operations(
    client: &MihomoClient,
    profile: &Path,
    home: &Path,
) {
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
                .groups()
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
                .groups()
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
            .groups()
            .iter()
            .all(|group| !group.hidden)
    );

    client
        .reload_config(profile, true)
        .await
        .expect("restore supplied profile after real proxy-operation verification");
}

pub(crate) async fn verify_catalog_apis(client: &MihomoClient) {
    let catalog = client.proxy_catalog().await.expect("GET /proxies");
    assert!(
        catalog.proxy_count > 0,
        "Mihomo did not expose built-in proxies"
    );
    if let Some(group) = catalog.groups().first() {
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
