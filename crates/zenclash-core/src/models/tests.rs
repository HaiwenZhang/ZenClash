use super::*;

#[test]
fn system_proxy_port_prefers_mixed_over_http() {
    let config = RuntimeConfig {
        port: 7892,
        mixed_port: 7890,
        ..RuntimeConfig::default()
    };

    assert_eq!(config.system_proxy_port(), Some(7890));
}

#[test]
fn system_proxy_port_rejects_a_socks_only_listener() {
    let config = RuntimeConfig {
        socks_port: 7891,
        ..RuntimeConfig::default()
    };

    assert_eq!(config.system_proxy_port(), None);
}

#[test]
fn decodes_real_mihomo_runtime_shapes() {
    let config: RuntimeConfig = serde_json::from_str(
        r#"{"port":7890,"socks-port":7891,"mode":"rule","log-level":"info","ipv6":true,"tun":{"enable":false,"stack":"mixed","auto-route":true},"sniffing":{"enable":true}}"#,
    )
    .unwrap();
    assert_eq!(config.socks_port, 7891);
    assert!(config.ipv6);
    assert_eq!(config.tun.stack, "mixed");
    assert!(config.sniffing.enable);

    let rules: RuleCatalog = serde_json::from_str(
        r#"{"rules":[{"type":"Domain","payload":"example.com","proxy":"DIRECT","size":-1}]}"#,
    )
    .unwrap();
    assert_eq!(rules.rules[0].kind, "Domain");
    assert_eq!(rules.rules[0].size, -1);

    let runtime_rules: RuleCatalog = serde_json::from_str(
        r#"{"rules":[{"type":"Domain","payload":"example.org","proxy":"Proxy","size":-1,"index":12,"extra":{"disabled":true,"hitCount":7,"hitAt":"2026-08-26T00:00:00Z","missCount":3,"missAt":"2026-08-25T00:00:00Z"}}]}"#,
    )
    .unwrap();
    assert_eq!(runtime_rules.rules[0].index, Some(12));
    assert_eq!(
        runtime_rules.rules[0]
            .extra
            .as_ref()
            .expect("rule runtime stats")
            .hit_count,
        7
    );
}

#[test]
fn decodes_null_mihomo_connection_collection_as_empty() {
    let snapshot: ConnectionsSnapshot =
        serde_json::from_str(r#"{"connections":null,"downloadTotal":12}"#).unwrap();

    assert!(snapshot.connections.is_empty());
    assert_eq!(snapshot.download_total, 12);
}

#[test]
fn decodes_rule_provider_conversion_metadata() {
    let catalog: ProviderCatalog = serde_json::from_str(
        r#"{"providers":{"domains":{"name":"domains","vehicleType":"HTTP","behavior":"domain","format":"mrs","ruleCount":42}}}"#,
    )
    .unwrap();

    let provider = catalog.providers.get("domains").unwrap();

    assert_eq!(provider.behavior, "domain");
    assert_eq!(provider.format, "mrs");
    assert_eq!(provider.rule_count, 42);
}
