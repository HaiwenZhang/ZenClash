use super::*;

#[test]
fn null_proxy_members_and_history_decode_as_empty_collections() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"empty":{"name":"empty","type":"Selector","all":null,"history":null}}}"#,
    )
    .unwrap();

    let proxy = &raw.proxies["empty"];
    assert!(proxy.all.is_empty());
    assert!(proxy.history.is_empty());
}

#[test]
fn catalog_keeps_group_controller_keys_independent_of_display_labels() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{
        "GLOBAL":{"name":"Global display label","type":"Selector","all":["first-key","second-key"]},
        "first-key":{"name":"Same label","type":"Selector","now":"DIRECT","all":["DIRECT"]},
        "second-key":{"name":"Same label","type":"Selector","now":"DIRECT","all":["DIRECT"]},
        "DIRECT":{"type":"Direct"}
    }}"#,
    )
    .unwrap();
    let mut catalog = ProxyCatalog::from(raw);
    let global: Vec<_> = catalog
        .groups_for_mode("global")
        .map(ProxyGroup::controller_name)
        .collect();
    assert_eq!(global, ["GLOBAL"]);
    let rule_keys: Vec<_> = catalog
        .groups_for_mode("rule")
        .map(ProxyGroup::controller_name)
        .collect();
    assert_eq!(rule_keys, ["first-key", "second-key"]);
    assert!(catalog.set_group_selection("second-key", "REJECT".into()));
    assert_eq!(
        catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == "first-key")
            .unwrap()
            .now,
        "DIRECT"
    );
    assert_eq!(
        catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == "second-key")
            .unwrap()
            .name,
        "Same label"
    );
}

#[test]
fn configured_group_order_is_preserved_instead_of_name_or_selector_priority() {
    // Mihomo's map keys are alphabetic; GLOBAL.all carries the configured
    // order, deliberately different from both alphabetic and selector order.
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{
            "GLOBAL":{"type":"Selector","all":["DIRECT","z-stream","a-backup","Proxy"]},
            "Proxy":{"type":"Selector","all":["DIRECT"]},
            "a-backup":{"type":"Selector","all":["DIRECT"]},
            "z-stream":{"type":"Selector","all":["DIRECT"]}
        }}"#,
    )
    .unwrap();
    let catalog = ProxyCatalog::from(raw);
    assert_eq!(
        catalog
            .groups_for_mode("rule")
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>(),
        ["z-stream", "a-backup", "Proxy"]
    );
}

#[test]
fn configured_order_uses_controller_keys_and_hidden_filter_keeps_relative_order() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{
            "GLOBAL":{"type":"Selector","all":["z-key","hidden","a-key"]},
            "a-key":{"name":"First alphabetically","type":"Selector","all":["DIRECT"]},
            "hidden":{"type":"Selector","hidden":true,"all":["DIRECT"]},
            "z-key":{"name":"Last alphabetically","type":"Selector","all":["DIRECT"]}
        }}"#,
    )
    .unwrap();
    let mut catalog = ProxyCatalog::from(raw);
    let names = |catalog: &ProxyCatalog| {
        catalog
            .groups_for_mode("rule")
            .map(|group| group.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&catalog),
        ["Last alphabetically", "hidden", "First alphabetically"]
    );
    catalog.hide_hidden_groups();
    assert_eq!(
        names(&catalog),
        ["Last alphabetically", "First alphabetically"]
    );
    let id = &catalog.groups_for_mode("rule").next().unwrap().all[0];
    assert_eq!(catalog.referencing_groups(id), [0, 1]);
}

#[test]
fn group_members_keep_declared_order_instead_of_controller_map_order() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{
            "Proxy":{"type":"Selector","all":["z-node","missing","a-node","z-node"]},
            "a-node":{"provider-name":"subscription"},
            "z-node":{"provider-name":"subscription"}
        }}"#,
    )
    .unwrap();
    let catalog = ProxyCatalog::from(raw);
    assert_eq!(
        catalog.groups()[0]
            .all
            .iter()
            .map(ProxyNodeId::controller_name)
            .collect::<Vec<_>>(),
        ["z-node", "missing", "a-node", "z-node"]
    );
}

#[test]
fn repeated_group_members_resolve_to_one_canonical_node_allocation() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"HK":{"name":"HK","type":"Shadowsocks","provider-name":"Airport A","history":[{"delay":100}]},"first":{"name":"first","type":"Selector","all":["HK"]},"second":{"name":"second","type":"Selector","all":["HK"]}}}"#,
    ).unwrap();
    let catalog = ProxyCatalog::from(raw);
    let first = catalog.node(&catalog.groups[0].all[0]).unwrap();
    let second = catalog.node(&catalog.groups[1].all[0]).unwrap();
    assert_eq!(first, second);
    assert!(
        std::ptr::eq(first, second),
        "one Mihomo node was allocated separately for each group"
    );
}

#[test]
fn canonical_identity_uses_controller_key_and_provider_and_preserves_missing_members() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"a":{"name":"HK","provider-name":"Airport A","history":[{"delay":42}]},"b":{"name":"HK","provider-name":"Airport B","history":[{"delay":77}]},"first":{"type":"Selector","all":["a","b","missing"]},"second":{"type":"Selector","all":["a","missing"]}}}"#,
    ).unwrap();
    let catalog = ProxyCatalog::from(raw);
    let first = &catalog.groups[0].all;
    let second = &catalog.groups[1].all;
    assert_ne!(first[0], first[1]);
    assert_eq!(first[0].controller_name(), "a");
    assert_eq!(first[0].provider(), Some("Airport A"));
    assert_eq!(catalog.node(&first[0]).unwrap().name, "HK");
    assert_eq!(catalog.node(&first[1]).unwrap().latest_delay(), Some(77));
    assert!(std::ptr::eq(
        catalog.node(&first[2]).unwrap(),
        catalog.node(&second[1]).unwrap()
    ));
    assert_eq!(catalog.node(&first[2]).unwrap().name, "missing");
    assert_eq!(catalog.referencing_groups(&first[0]), [0, 1]);
}

#[test]
fn hiding_groups_rebases_the_prepared_reference_indices() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"HK":{"history":[{"delay":42}]},"GLOBAL":{"type":"Selector","hidden":true,"all":["HK"]},"visible":{"type":"Selector","all":["HK"]}}}"#,
    ).unwrap();
    let mut catalog = ProxyCatalog::from(raw);
    let id = catalog.groups[0].all[0].clone();
    assert_eq!(catalog.referencing_groups(&id), [0, 1]);
    catalog.hide_hidden_groups();
    assert_eq!(catalog.groups.len(), 1);
    assert_eq!(catalog.groups[0].name, "visible");
    assert_eq!(catalog.referencing_groups(&id), [0]);
}

#[test]
fn resolves_group_members_from_mihomo_catalog() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{
            "proxies": {
                "DIRECT": {"name":"DIRECT","type":"Direct","alive":true,"udp":true,"history":[]},
                "HK 01": {"name":"HK 01","type":"Shadowsocks","alive":true,"history":[{"time":"now","delay":42}]},
                "Proxy": {"name":"Proxy","type":"Selector","now":"HK 01","all":["HK 01","DIRECT"],"test-url":"https://example.com"}
            }
        }"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);
    assert_eq!(catalog.proxy_count, 3);
    assert_eq!(catalog.groups.len(), 1);
    assert_eq!(catalog.groups[0].name, "Proxy");
    assert_eq!(catalog.groups[0].now, "HK 01");
    assert_eq!(
        catalog
            .node(&catalog.groups[0].all[0])
            .unwrap()
            .latest_delay(),
        Some(42)
    );
    assert_eq!(
        catalog
            .node(&catalog.groups[0].all[1])
            .unwrap()
            .capabilities()
            .collect::<Vec<_>>(),
        vec!["UDP"]
    );
}

#[test]
fn empty_group_test_url_uses_the_client_default() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"],"testUrl":""}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(catalog.groups[0].test_url, None);
}

#[test]
fn automatic_group_preserves_fixed_state() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Auto":{"name":"Auto","type":"URLTest","now":"DIRECT","all":["DIRECT"],"fixed":true}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog.groups[0].behavior,
        ProxyGroupBehavior::Automatic { fixed: true }
    );
}

#[test]
fn automatic_group_accepts_real_mihomo_fixed_member_encoding() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Auto":{"name":"Auto","type":"URLTest","now":"DIRECT","all":["DIRECT"],"fixed":"DIRECT"}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog.groups[0].behavior,
        ProxyGroupBehavior::Automatic { fixed: true }
    );
}

#[test]
fn selector_group_has_explicit_selection_behavior() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(catalog.groups[0].behavior, ProxyGroupBehavior::Selector);
}

#[test]
fn fallback_group_without_a_pin_remains_automatic() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Fallback":{"name":"Fallback","type":"Fallback","now":"DIRECT","all":["DIRECT"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog.groups[0].behavior,
        ProxyGroupBehavior::Automatic { fixed: false }
    );
}

#[test]
fn load_balance_group_has_non_selectable_behavior() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Balance":{"name":"Balance","type":"LoadBalance","now":"DIRECT","all":["DIRECT"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(catalog.groups[0].behavior, ProxyGroupBehavior::LoadBalance);
}

#[test]
fn unknown_group_behavior_preserves_the_mihomo_type() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Future":{"name":"Future","type":"ConsistentHashV2","now":"DIRECT","all":["DIRECT"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog.groups[0].behavior,
        ProxyGroupBehavior::Unknown("ConsistentHashV2".into())
    );
}

#[test]
fn hidden_group_state_is_preserved_for_visibility_filtering() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Internal":{"name":"Internal","type":"Selector","now":"DIRECT","all":["DIRECT"],"hidden":true}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert!(catalog.groups[0].hidden);
}

#[test]
fn blank_provider_name_uses_the_regular_proxy_delay_endpoint() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct","provider-name":"  "},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog
            .node(&catalog.groups[0].all[0])
            .unwrap()
            .provider_name,
        None
    );
}

#[test]
fn provider_name_is_trimmed_without_losing_provider_routing() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"HK":{"name":"HK","type":"Shadowsocks","provider-name":" Airport A "},"Proxy":{"name":"Proxy","type":"Selector","now":"HK","all":["HK"]}}}"#,
    )
    .unwrap();

    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog
            .node(&catalog.groups[0].all[0])
            .unwrap()
            .provider_name
            .as_deref(),
        Some("Airport A")
    );
}

#[test]
fn rule_mode_excludes_the_synthetic_global_group() {
    let catalog = mode_catalog();

    let names = catalog
        .groups_for_mode("rule")
        .map(|group| group.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["Proxy", "Streaming"]);
}

#[test]
fn global_mode_exposes_only_the_synthetic_global_group() {
    let catalog = mode_catalog();

    let names = catalog
        .groups_for_mode("GLOBAL")
        .map(|group| group.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["GLOBAL"]);
}

#[test]
fn direct_mode_has_no_selectable_proxy_groups() {
    let catalog = mode_catalog();

    assert_eq!(catalog.groups_for_mode("direct").count(), 0);
}

#[test]
fn missing_configured_order_does_not_guess_a_primary_selector() {
    let raw: RawProxyCatalog = serde_json::from_str(
        r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"OneDrive":{"name":"OneDrive","type":"Selector","all":["DIRECT"]},"🔰 选择节点":{"name":"🔰 选择节点","type":"Selector","all":["DIRECT"]},"GLOBAL":{"name":"GLOBAL","type":"Selector","all":["DIRECT"]}}}"#,
    )
    .unwrap();
    let catalog = ProxyCatalog::from(raw);

    assert_eq!(
        catalog
            .groups_for_mode("rule")
            .next()
            .map(|group| group.name.as_str()),
        Some("OneDrive")
    );
}

fn mode_catalog() -> ProxyCatalog {
    ProxyCatalog::from_group_nodes(
        ["GLOBAL", "Proxy", "Streaming"]
            .into_iter()
            .map(|name| {
                (
                    ProxyGroup {
                        name: name.into(),
                        ..ProxyGroup::default()
                    },
                    Vec::new(),
                )
            })
            .collect(),
        3,
    )
}
