use super::*;

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
}

#[test]
fn decodes_null_mihomo_connection_collection_as_empty() {
    let snapshot: ConnectionsSnapshot =
        serde_json::from_str(r#"{"connections":null,"downloadTotal":12}"#).unwrap();

    assert!(snapshot.connections.is_empty());
    assert_eq!(snapshot.download_total, 12);
}
