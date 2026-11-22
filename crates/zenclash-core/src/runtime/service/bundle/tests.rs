use super::test_support::TestHome;
use super::*;

#[tokio::test]
async fn merged_provider_and_tls_are_held_before_sources_disappear() {
    let home = TestHome::new();
    std::fs::write(home.0.join("cert.pem"), b"held merged certificate").unwrap();
    std::fs::write(home.0.join("nodes.yaml"), "proxies: []\n").unwrap();
    let payload = "defaults: &base\n  proxy-providers:\n    local: {type: file, path: nodes.yaml}\n  proxies:\n  - {name: local, type: http, server: localhost, port: 8080, certificate: cert.pem}\nnext: &next {<<: *base}\n<<: *next\n";
    let bundle = ServiceRuntimeBundle::prepare(payload, home.0.clone())
        .await
        .unwrap();
    std::fs::remove_file(home.0.join("cert.pem")).unwrap();
    std::fs::remove_file(home.0.join("nodes.yaml")).unwrap();
    let document: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
    let provider = document["proxy-providers"]["local"]["path"]
        .as_str()
        .unwrap();
    let certificate = document["proxies"][0]["certificate"].as_str().unwrap();
    assert!(
        bundle
            .assets
            .iter()
            .any(|asset| asset.path == provider && asset.bytes.as_ref() == b"proxies: []\n")
    );
    assert!(bundle.assets.iter().any(
        |asset| asset.path == certificate && asset.bytes.as_ref() == b"held merged certificate"
    ));
}

#[test]
fn exported_final_budget_does_not_include_superseded_http_seeds() {
    let providers: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|name| CacheProvider {
            kind: ProviderKind::Rule,
            path: format!("assets/providers/{name}"),
        })
        .collect();
    let bundle = ServiceRuntimeBundle {
        yaml: String::new(),
        assets: providers
            .iter()
            .zip([1, MAX_TOTAL_BYTES / 2, MAX_TOTAL_BYTES / 2 - 1])
            .map(|(provider, size)| RuntimeAsset {
                path: provider.path.clone(),
                bytes: vec![0; size].into(),
            })
            .collect(),
    };
    let mut exported = bundle.cache_export_base(&providers);
    exported
        .replace_provider_cache(&providers[0], Some(vec![1, 2]))
        .expect("a two-byte final cache must not be charged for superseded seeds");
    exported
        .replace_provider_cache(&providers[1], None)
        .unwrap();
    exported
        .replace_provider_cache(&providers[2], None)
        .unwrap();
    assert_eq!(exported.assets.len(), 1);
    assert_eq!(exported.assets[0].bytes.as_ref(), [1, 2]);
    assert_eq!(bundle.assets.len(), 3);
    assert_eq!(
        bundle
            .assets
            .iter()
            .map(|asset| asset.bytes.len())
            .sum::<usize>(),
        MAX_TOTAL_BYTES
    );
}

#[tokio::test]
async fn startup_geodata_preserves_packaged_metadb_bytes_without_rewriting_source() {
    let home = TestHome::new();
    let bytes = b"packaged maxmind data";
    std::fs::write(home.0.join("geoip.metadb"), bytes).unwrap();
    let bundle = ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
        .await
        .unwrap();
    let mmdb = bundle
        .assets
        .iter()
        .find(|asset| asset.path == "assets/geodata/country.mmdb")
        .unwrap();
    assert_eq!(mmdb.bytes.as_ref(), bytes);
    assert_eq!(std::fs::read(home.0.join("geoip.metadb")).unwrap(), bytes);
    assert!(!home.0.join("country.mmdb").exists());
}

#[tokio::test]
async fn startup_geodata_uses_mihomo_sorted_case_insensitive_mmdb_priority() {
    let home = TestHome::new();
    std::fs::write(home.0.join("Country.mmdb"), b"first sorted alias").unwrap();
    std::fs::write(home.0.join("geoip.db"), b"second alias").unwrap();
    std::fs::write(home.0.join("geoip.metadb"), b"packaged alias").unwrap();
    let bundle = ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
        .await
        .unwrap();
    let mmdb = bundle
        .assets
        .iter()
        .find(|asset| asset.path == "assets/geodata/country.mmdb")
        .unwrap();
    assert_eq!(mmdb.bytes.as_ref(), b"first sorted alias");
}

#[tokio::test]
async fn startup_geodata_skips_alias_directories_and_selects_existing_db() {
    let home = TestHome::new();
    std::fs::create_dir(home.0.join("Country.mmdb")).unwrap();
    std::fs::write(home.0.join("geoip.db"), b"selected db").unwrap();
    std::fs::write(home.0.join("geoip.metadb"), b"later alias").unwrap();
    let bundle = ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
        .await
        .unwrap();
    let mmdb = bundle
        .assets
        .iter()
        .find(|asset| asset.path == "assets/geodata/country.mmdb")
        .unwrap();
    assert_eq!(mmdb.bytes.as_ref(), b"selected db");
}

#[tokio::test]
async fn prepares_provider_and_nested_tls_without_rewriting_user_files() {
    let home = TestHome::new();
    let provider = "proxies:\n- name: node\n  type: http\n  certificate: cert.pem\n";
    std::fs::write(home.0.join("nodes.yaml"), provider).unwrap();
    std::fs::write(home.0.join("cert.pem"), b"public certificate").unwrap();
    std::fs::write(home.0.join("GeoIP.dat"), b"geodata").unwrap();
    let yaml = "proxy-providers:\n  local:\n    type: file\n    path: nodes.yaml\n";
    let bundle = ServiceRuntimeBundle::prepare(yaml, home.0.clone())
        .await
        .unwrap();
    let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
    let path = value["proxy-providers"]["local"]["path"].as_str().unwrap();
    assert!(path.starts_with("assets/providers/"));
    let provider_asset = bundle
        .assets
        .iter()
        .find(|asset| asset.path == path)
        .unwrap();
    let provider_yaml: Value = serde_yaml::from_slice(&provider_asset.bytes).unwrap();
    assert!(
        provider_yaml["proxies"][0]["certificate"]
            .as_str()
            .unwrap()
            .starts_with("assets/tls/")
    );
    assert!(
        bundle
            .assets
            .iter()
            .any(|asset| asset.path == "assets/geodata/GeoIP.dat")
    );
    assert_eq!(
        std::fs::read_to_string(home.0.join("nodes.yaml")).unwrap(),
        provider
    );
}

#[tokio::test]
async fn exported_http_cache_replaces_only_its_prepared_resource() {
    let home = TestHome::new();
    std::fs::write(home.0.join("rules.txt"), b"old cache").unwrap();
    std::fs::write(home.0.join("cert.pem"), b"certificate").unwrap();
    let payload = "rule-providers:\n  rules:\n    type: http\n    url: https://example.com/rules\n    path: rules.txt\nproxies:\n- name: p\n  type: http\n  certificate: cert.pem\n";
    let original = ServiceRuntimeBundle::prepare(payload, home.0.clone())
        .await
        .unwrap();
    let mut exported = original.clone();
    let providers = exported.cache_providers().unwrap();
    assert_eq!(providers.len(), 1);
    exported
        .replace_provider_cache(&providers[0], Some(b"new cache".to_vec()))
        .unwrap();
    let bytes = |bundle: &ServiceRuntimeBundle| {
        bundle
            .assets
            .iter()
            .find(|asset| asset.path == providers[0].path)
            .unwrap()
            .bytes
            .clone()
    };
    assert_eq!(bytes(&original).as_ref(), b"old cache");
    assert_eq!(bytes(&exported).as_ref(), b"new cache");
    assert_eq!(exported.yaml, original.yaml);
    assert!(
        exported
            .assets
            .iter()
            .any(|asset| asset.bytes.as_ref() == b"certificate")
    );
    assert_eq!(
        std::fs::read(home.0.join("rules.txt")).unwrap(),
        b"old cache"
    );
}

#[tokio::test]
async fn exported_absent_cache_removes_seed_while_empty_cache_retains_empty_asset() {
    let home = TestHome::new();
    std::fs::write(home.0.join("rules.txt"), b"seed").unwrap();
    let payload = "rule-providers:\n  rules:\n    type: http\n    url: https://example.com/rules\n    path: rules.txt\n";
    let original = ServiceRuntimeBundle::prepare(payload, home.0.clone())
        .await
        .unwrap();
    let providers = original.cache_providers().unwrap();
    let mut absent = original.clone();
    absent.replace_provider_cache(&providers[0], None).unwrap();
    assert!(
        !absent
            .assets
            .iter()
            .any(|asset| asset.path == providers[0].path)
    );
    let mut empty = original.clone();
    empty
        .replace_provider_cache(&providers[0], Some(vec![]))
        .unwrap();
    assert!(
        empty
            .assets
            .iter()
            .any(|asset| asset.path == providers[0].path && asset.bytes.is_empty())
    );
    assert_eq!(original.assets.len(), 1);
}

#[tokio::test]
async fn exported_cache_declarations_exclude_file_and_inline_providers() {
    let home = TestHome::new();
    std::fs::write(home.0.join("rules.txt"), b"rules").unwrap();
    let payload = "rule-providers:\n  file:\n    type: file\n    path: rules.txt\n  inline:\n    type: inline\n    payload: []\n  remote:\n    url: https://example.com/rules\n";
    let bundle = ServiceRuntimeBundle::prepare(payload, home.0.clone())
        .await
        .unwrap();
    let providers = bundle.cache_providers().unwrap();
    assert_eq!(providers.len(), 1);
    let yaml: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
    assert_eq!(
        providers[0].path,
        yaml["rule-providers"]["remote"]["path"].as_str().unwrap()
    );
    assert_eq!(providers[0].kind, ProviderKind::Rule);
}

#[tokio::test]
async fn missing_file_provider_rejects_before_any_service_operation() {
    let home = TestHome::new();
    let result = ServiceRuntimeBundle::prepare(
        "proxy-providers:\n  local:\n    type: file\n    path: missing.yaml\n",
        home.0.clone(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn absent_http_cache_gets_a_safe_destination_without_a_fake_asset() {
    let home = TestHome::new();
    let bundle = ServiceRuntimeBundle::prepare(
        "proxy-providers:\n  remote:\n    type: http\n    url: https://example.com/nodes\n",
        home.0.clone(),
    )
    .await
    .unwrap();
    let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
    assert!(
        value["proxy-providers"]["remote"]["path"]
            .as_str()
            .unwrap()
            .starts_with("assets/providers/")
    );
    assert!(bundle.assets.is_empty());
}

#[tokio::test]
async fn inline_pem_remains_data_and_oversized_assets_are_rejected() {
    let home = TestHome::new();
    let bundle = ServiceRuntimeBundle::prepare(
        "proxies:\n- certificate: |\n    -----BEGIN CERTIFICATE-----\n    encoded\n",
        home.0.clone(),
    )
    .await
    .unwrap();
    assert!(bundle.assets.is_empty());
    let file = std::fs::File::create(home.0.join("large.pem")).unwrap();
    file.set_len(MAX_ASSET_BYTES + 1).unwrap();
    assert!(
        ServiceRuntimeBundle::prepare("proxies:\n- certificate: large.pem\n", home.0.clone())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn wireguard_masque_and_inline_ca_keys_are_preserved_as_data() {
    let home = TestHome::new();
    let yaml = "tls:\n  custom-certifactes: [inline-DER]\nproxies:\n- type: wireguard\n  private-key: base64-wg\n- type: masque\n  private-key: base64-ec\n- type: openvpn\n  ca: inline-ca\n";
    let bundle = ServiceRuntimeBundle::prepare(yaml, home.0.clone())
        .await
        .unwrap();
    let value: Value = serde_yaml::from_str(bundle.yaml()).unwrap();
    assert_eq!(
        value["proxies"][0]["private-key"].as_str(),
        Some("base64-wg")
    );
    assert_eq!(
        value["proxies"][1]["private-key"].as_str(),
        Some("base64-ec")
    );
    assert_eq!(value["proxies"][2]["ca"].as_str(), Some("inline-ca"));
    assert!(bundle.assets.is_empty());
}
#[tokio::test]
async fn accepted_delta_shares_asset_bytes_after_source_deletion() {
    let home = TestHome::new();
    std::fs::write(home.0.join("provider.yaml"), "payload: [example.org]\n").unwrap();
    let bundle=ServiceRuntimeBundle::prepare("mode: rule\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n",home.0.clone()).await.unwrap();
    std::fs::remove_file(home.0.join("provider.yaml")).unwrap();
    let next = bundle
        .with_delta(&serde_json::json!({"mode":"global"}))
        .unwrap();
    assert!(next.yaml().contains("mode: global"));
    assert_eq!(bundle.assets.len(), 1);
    assert!(Arc::ptr_eq(&bundle.assets[0].bytes, &next.assets[0].bytes));
    assert_eq!(bundle.assets[0].path, next.assets[0].path);
}

#[tokio::test]
async fn geodata_scanning_matches_case_insensitively() {
    let home = TestHome::new();
    std::fs::write(home.0.join("geoip.dat"), b"geoip-bytes").unwrap();
    std::fs::write(home.0.join("GeoSite.dat"), b"geosite-bytes").unwrap();
    std::fs::write(home.0.join("asn.mmdb"), b"asn-bytes").unwrap();
    let bundle = ServiceRuntimeBundle::prepare("mode: rule\n", home.0.clone())
        .await
        .unwrap();
    let paths: Vec<&str> = bundle
        .assets
        .iter()
        .map(|asset| asset.path.as_str())
        .collect();
    assert!(paths.contains(&"assets/geodata/GeoIP.dat"), "{paths:?}");
    assert!(paths.contains(&"assets/geodata/geosite.dat"), "{paths:?}");
    assert!(paths.contains(&"assets/geodata/ASN.mmdb"), "{paths:?}");
}
