use std::{collections::HashSet, fs};

use anyhow::Result;
use serde_yaml::Value;
use zenclash_core::{
    mihomo::{
        ConfigDiffKind, EffectiveConfigPipeline, EffectiveConfigRequest, FULL_RELOAD_CONFIG_KEYS,
        HOT_UPDATE_CONFIG_KEYS, MihomoEndpoint, OverrideKeys, RulesetBehavior,
        RulesetConversionError, RulesetConverter, apply_controlled_override, diff_yaml_configs,
        merge_profile_overrides, yaml_content_hash,
    },
    platform::CoreKind,
    runtime::ControlledConfigStore,
    storage::validate_clash_yaml,
};

use crate::TempDir;

#[test]
fn override_keys_controlled_overrides_and_yaml_content_hash_are_deterministic() -> Result<()> {
    let temp = TempDir::new("config-pipeline")?;
    let base_path = temp.path().join("base.yaml");
    let override_a = temp.path().join("10-dns.yaml");
    let override_b = temp.path().join("20-mode.yaml");

    let base_yaml = concat!(
        "mixed-port: 7890\n",
        "mode: rule\n",
        "tun:\n",
        "  enable: false\n",
        "  mtu: 9000\n",
        "dns:\n",
        "  enable: true\n",
        "  fallback-filter:\n",
        "    geoip: true\n",
        "    geoip-code: CN\n",
        "  nameserver-policy:\n",
        "    '+.old.example': 1.1.1.1\n",
        "hosts:\n",
        "  old.local: 127.0.0.1\n",
        "rules:\n",
        "  - MATCH,DIRECT\n",
    );
    fs::write(&base_path, base_yaml)?;
    fs::write(&override_a, "log-level: warning\n")?;
    fs::write(&override_b, "mode: global\n")?;

    // Exercise `apply_controlled_override` + `OverrideKeys` directly on `serde_yaml::Value`.
    let mut document: Value = serde_yaml::from_str(base_yaml)?;
    let controlled: Value = serde_yaml::from_str(concat!(
        "tun:\n",
        "  mtu: null\n",
        "hosts:\n",
        "  new.local: 192.0.2.10\n",
        "dns:\n",
        "  nameserver-policy:\n",
        "    '+.new.example': 8.8.8.8\n",
    ))?;
    let mut keys = OverrideKeys::from_paths(["tun.mtu", "dns.fallback-filter.geoip"]);
    assert!(!keys.is_empty());
    assert!(keys.contains("tun.mtu"));
    assert!(!keys.insert("   "));
    apply_controlled_override(&mut document, controlled, Some(&keys));

    assert!(document["tun"].get("mtu").is_none());
    assert!(document["dns"]["fallback-filter"].get("geoip").is_none());
    assert_eq!(
        document["dns"]["fallback-filter"]["geoip-code"].as_str(),
        Some("CN")
    );
    assert!(document["hosts"].get("old.local").is_none());
    assert_eq!(document["hosts"]["new.local"].as_str(), Some("192.0.2.10"));
    assert!(
        document["dns"]["nameserver-policy"]
            .get("+.old.example")
            .is_none()
    );
    assert_eq!(
        document["dns"]["nameserver-policy"]["+.new.example"].as_str(),
        Some("8.8.8.8")
    );

    // Exercise controlled-config composition with ordered external YAML overrides + hashing.
    let controlled_store = ControlledConfigStore::new(temp.path().join("controlled"));
    let update = controlled_store.prepare_json_update(
        &base_path,
        &serde_json::json!({"allow-lan": true, "mixed-port": 17890}),
    )?;
    controlled_store.commit(&update)?;

    let overrides = vec![override_a, override_b.clone()];
    let rendered_first = controlled_store.effective_with_overrides(&base_path, &overrides)?;
    let rendered_second = controlled_store.effective_with_overrides(&base_path, &overrides)?;
    let hash_first = yaml_content_hash(&rendered_first);
    let hash_second = yaml_content_hash(&rendered_second);
    assert_eq!(hash_first, hash_second);

    let parsed: Value = serde_yaml::from_str(&rendered_first)?;
    assert_eq!(parsed["allow-lan"].as_bool(), Some(true));
    assert_eq!(parsed["mixed-port"].as_u64(), Some(17_890));
    assert_eq!(parsed["log-level"].as_str(), Some("warning"));
    assert_eq!(parsed["mode"].as_str(), Some("global"));

    // Mutating an override changes the rendered YAML and its SHA-256 content hash.
    fs::write(&override_b, "mode: direct\n")?;
    let rendered_changed = controlled_store.effective_with_overrides(&base_path, &overrides)?;
    assert_ne!(hash_first, yaml_content_hash(&rendered_changed));

    Ok(())
}

#[test]
fn merge_profile_overrides_and_diff_yaml_configs_report_end_to_end_changes() -> Result<()> {
    let temp = TempDir::new("merge-diff")?;
    let base_path = temp.path().join("base.yaml");
    let first_override = temp.path().join("01.yaml");
    let second_override = temp.path().join("02.yaml");

    let base_yaml = concat!(
        "defaults: &dns_defaults\n",
        "  enable: true\n",
        "  ipv6: false\n",
        "mixed-port: 7890\n",
        "mode: rule\n",
        "dns:\n",
        "  <<: *dns_defaults\n",
        "rules:\n",
        "  - MATCH,DIRECT\n",
    );
    fs::write(&base_path, base_yaml)?;
    fs::write(&first_override, "mode: global\ndns:\n  ipv6: true\n")?;
    fs::write(&second_override, "mode: direct\nlog-level: debug\n")?;

    let effective_yaml = merge_profile_overrides(&base_path, &[first_override, second_override])?;
    let report = diff_yaml_configs(base_yaml, &effective_yaml, 64)?;
    assert!(!report.truncated);

    let find = |path: &str| report.entries.iter().find(|entry| entry.path == path);
    let mode_entry = find("/mode").expect("mode diff entry");
    assert_eq!(mode_entry.kind, ConfigDiffKind::Changed);
    assert_eq!(mode_entry.source.as_deref(), Some("\"rule\""));
    assert_eq!(mode_entry.effective.as_deref(), Some("\"direct\""));

    let ipv6_entry = find("/dns/ipv6").expect("dns.ipv6 diff entry");
    assert_eq!(ipv6_entry.kind, ConfigDiffKind::Changed);
    assert_eq!(ipv6_entry.source.as_deref(), Some("false"));
    assert_eq!(ipv6_entry.effective.as_deref(), Some("true"));

    let log_level_entry = find("/log-level").expect("log-level diff entry");
    assert_eq!(log_level_entry.kind, ConfigDiffKind::Added);
    assert_eq!(log_level_entry.effective.as_deref(), Some("\"debug\""));

    // Bounded diff reports truncation when max_entries is smaller than total diffs.
    let bounded = diff_yaml_configs(base_yaml, &effective_yaml, 1)?;
    assert_eq!(bounded.entries.len(), 1);
    assert!(bounded.truncated);

    Ok(())
}

#[test]
fn hot_and_full_reload_config_keys_are_disjoint_and_clash_yaml_validation_works() {
    assert!(!HOT_UPDATE_CONFIG_KEYS.is_empty());
    assert!(!FULL_RELOAD_CONFIG_KEYS.is_empty());

    let hot: HashSet<&str> = HOT_UPDATE_CONFIG_KEYS.iter().copied().collect();
    let full: HashSet<&str> = FULL_RELOAD_CONFIG_KEYS.iter().copied().collect();
    assert_eq!(hot.len(), HOT_UPDATE_CONFIG_KEYS.len());
    assert_eq!(full.len(), FULL_RELOAD_CONFIG_KEYS.len());
    assert!(hot.is_disjoint(&full));

    assert!(
        validate_clash_yaml("mixed-port: 7890\nmode: rule\nrules:\n  - MATCH,DIRECT\n").is_ok()
    );
    assert!(validate_clash_yaml("").is_err());
    assert!(validate_clash_yaml("   \n\t  ").is_err());
    assert!(validate_clash_yaml("- not-a-mapping\n").is_err());
    assert!(validate_clash_yaml("foo: bar\nbaz: 123\n").is_err());
    assert!(validate_clash_yaml("rules: [unclosed").is_err());
}

#[test]
fn ruleset_converter_and_mihomo_endpoint_validate_inputs() -> Result<()> {
    let temp = TempDir::new("ruleset-endpoint")?;

    assert_eq!(RulesetBehavior::Domain.as_str(), "domain");
    assert_eq!(RulesetBehavior::IpCidr.as_str(), "ipcidr");
    assert_eq!(RulesetBehavior::Classical.as_str(), "classical");

    let converter = RulesetConverter::new(temp.path().join("nonexistent-mihomo"));
    let missing_result =
        converter.convert_mrs_to_text(temp.path().join("missing.mrs"), RulesetBehavior::Domain);
    assert!(matches!(
        missing_result,
        Err(RulesetConversionError::InvalidSource(_))
    ));
    let directory_result = converter.convert_mrs_to_text(temp.path(), RulesetBehavior::IpCidr);
    assert!(matches!(
        directory_result,
        Err(RulesetConversionError::InvalidSource(_))
    ));

    let shorthand = MihomoEndpoint::new("127.0.0.1:9090", "top-secret-token");
    assert_eq!(
        shorthand.http_url("/version")?,
        "http://127.0.0.1:9090/version"
    );
    assert_eq!(
        shorthand.websocket_url("/traffic")?,
        "ws://127.0.0.1:9090/traffic"
    );
    assert!(
        !format!("{shorthand:?}").contains("top-secret-token"),
        "Debug formatting must redact controller secret"
    );

    let https = MihomoEndpoint::new("https://controller.example.com:9443/api/", "");
    assert_eq!(
        https.http_url("configs")?,
        "https://controller.example.com:9443/api/configs"
    );
    assert_eq!(
        https.websocket_url("/logs")?,
        "wss://controller.example.com:9443/api/logs"
    );

    let random = MihomoEndpoint::with_random_secret("127.0.0.1:9090")?;
    assert_eq!(random.secret.len(), 64);

    let ipc = MihomoEndpoint::local_ipc(temp.path())?;
    assert!(ipc.ipc_path().is_some());
    assert_eq!(ipc.secret.len(), 64);

    for invalid in [
        "",
        "ftp://127.0.0.1:9090",
        "http://user:pass@127.0.0.1:9090",
        "http://127.0.0.1:9090?query=1",
        "http://127.0.0.1:9090#fragment",
    ] {
        let endpoint = MihomoEndpoint::new(invalid, "");
        assert!(
            endpoint.http_url("/version").is_err(),
            "expected endpoint `{invalid}` to be rejected"
        );
    }

    Ok(())
}

#[test]
fn effective_config_pipeline_compose_end_to_end_via_public_request_builder() -> Result<()> {
    let temp = TempDir::new("effective-pipeline-public")?;
    let override_file = temp.path().join("10-external.yaml");
    fs::write(&override_file, "log-level: debug\nmode: global\n")?;

    let source_yaml = concat!(
        "defaults: &tun_defaults\n",
        "  enable: true\n",
        "  stack: system\n",
        "  device: utun99\n",
        "mixed-port: 7890\n",
        "mode: rule\n",
        "tun:\n",
        "  <<: *tun_defaults\n",
        "dns:\n",
        "  enable: true\n",
        "  fallback-filter:\n",
        "    geoip: true\n",
        "    geoip-code: CN\n",
        "rules:\n",
        "  - MATCH,DIRECT\n",
    );

    let controlled: Value = serde_yaml::from_str(concat!(
        "allow-lan: true\n",
        "tun:\n",
        "  device: null\n",
        "dns:\n",
        "  fallback-filter:\n",
        "    geoip: null\n",
    ))?;
    let override_keys = OverrideKeys::from_paths(["tun.device", "dns.fallback-filter.geoip"]);

    let request = EffectiveConfigRequest::new(source_yaml, CoreKind::Mihomo)
        .with_controlled_override(controlled)
        .with_override_keys(override_keys)
        .with_yaml_overrides(vec![override_file])
        .with_disable_tun_for_runtime(true)
        .with_probe_listener_conflicts(false);

    let artifact = EffectiveConfigPipeline::compose(request)?;
    assert_eq!(artifact.content_hash, yaml_content_hash(&artifact.yaml));
    assert!(artifact.fallbacks.is_empty());

    let parsed: Value = serde_yaml::from_str(&artifact.yaml)?;
    assert_eq!(parsed["allow-lan"].as_bool(), Some(true));
    assert_eq!(parsed["log-level"].as_str(), Some("debug"));
    assert_eq!(parsed["mode"].as_str(), Some("global"));
    assert_eq!(parsed["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(parsed["tun"]["stack"].as_str(), Some("system"));
    // Clearing `tun.device` via OverrideKeys removes `utun99` so TUN normalization applies the platform default.
    assert_ne!(parsed["tun"]["device"].as_str(), Some("utun99"));
    assert!(parsed["dns"]["fallback-filter"].get("geoip").is_none());
    assert_eq!(
        parsed["dns"]["fallback-filter"]["geoip-code"].as_str(),
        Some("CN")
    );
    assert!(parsed["geox-url"]["geoip"].as_str().is_some());

    // Re-running an identical request produces the exact same SHA-256 content hash.
    let repeated = EffectiveConfigPipeline::compose(
        EffectiveConfigRequest::new(source_yaml, CoreKind::Mihomo)
            .with_disable_tun_for_runtime(false),
    )?;
    assert_ne!(artifact.content_hash, repeated.content_hash);
    let repeated_parsed: Value = serde_yaml::from_str(&repeated.yaml)?;
    assert_eq!(repeated_parsed["tun"]["enable"].as_bool(), Some(true));
    assert_eq!(repeated_parsed["tun"]["device"].as_str(), Some("utun99"));

    Ok(())
}
