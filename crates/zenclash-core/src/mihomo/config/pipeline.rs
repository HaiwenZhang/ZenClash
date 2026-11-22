//! Single-`Value`-stage effective configuration synthesis pipeline.
//!
//! Replaces multi-roundtrip YAML parse/serialize sequences with a single
//! in-memory [`serde_yaml::Value`] pipeline:
//!
//! 1. Parse source YAML once and expand `<<` YAML merge keys.
//! 2. Apply controlled override (including explicit `Value::Null` key clearing
//!    and complete-value replacement for `hosts` / `dns.nameserver-policy`).
//! 3. Parse, expand `<<` merges, and overlay ordered YAML override files.
//! 4. Normalize TUN defaults and GeoX URLs in place on `Value`.
//! 5. Project runtime policy (optional non-elevated Local TUN disable) and
//!    session listener fallbacks in place on `Value`.
//! 6. Validate and serialize **once** to the final YAML string + SHA-256 content hash.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use serde_yaml::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    ControlledConfigError, ControlledConfigResult, CoreKind, ListenerPortFallback,
    mihomo::config::listener_fallback::{
        SessionListenerFallback, apply_session_fallbacks_to_value, resolve_conflicts,
    },
    mihomo::config::merge::{
        expand_yaml_merges, merge_controlled_yaml, merge_value_overrides, serialize_profile,
    },
    platform::fs_io::MAX_PROFILE_BYTES,
};

const MIHOMO_GEOX_URLS: [(&str, &str); 3] = [
    (
        "geoip",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.dat",
    ),
    (
        "geosite",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geosite.dat",
    ),
    (
        "mmdb",
        "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@release/geoip.metadb",
    ),
];

pub(crate) fn normalize_runtime_value(
    _kind: CoreKind,
    document: &mut Value,
) -> ControlledConfigResult<bool> {
    let root = document
        .as_mapping_mut()
        .ok_or(ControlledConfigError::NotMapping)?;
    let tun_changed = crate::mihomo::tun::config::normalize(root);
    let geox_changed = insert_missing_mihomo_geox_urls(root);
    Ok(tun_changed || geox_changed)
}

pub(crate) fn normalize_runtime_payload(
    kind: CoreKind,
    payload: String,
) -> ControlledConfigResult<String> {
    let mut document = serde_yaml::from_str::<Value>(&payload)?;
    if !normalize_runtime_value(kind, &mut document)? {
        return Ok(payload);
    }
    let normalized = serde_yaml::to_string(&document)?;
    if normalized.len() > MAX_PROFILE_BYTES {
        return Err(ControlledConfigError::TooLarge);
    }
    Ok(normalized)
}

fn insert_missing_mihomo_geox_urls(root: &mut serde_yaml::Mapping) -> bool {
    let value = root
        .entry(Value::String("geox-url".into()))
        .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
    if value.is_null() {
        *value = Value::Mapping(serde_yaml::Mapping::new());
    }
    let Value::Mapping(geox_urls) = value else {
        return false;
    };
    let mut changed = false;
    for (name, url) in MIHOMO_GEOX_URLS {
        let key = Value::String(name.into());
        if !geox_urls.contains_key(&key) {
            geox_urls.insert(key, Value::String(url.into()));
            changed = true;
        }
    }
    changed
}

/// Controlled configuration keys eligible for in-place `PATCH /configs` hot updates
/// without reloading listeners or restarting the core.
///
/// Strictly disjoint from [`FULL_RELOAD_CONFIG_KEYS`].
pub const HOT_UPDATE_CONFIG_KEYS: &[&str] = &[
    "mode",
    "log-level",
    "allow-lan",
    "bind-address",
    "ipv6",
    "port",
    "socks-port",
    "mixed-port",
    "redir-port",
    "tproxy-port",
    "tun.enable",
    "tun.device",
    "tun.stack",
    "tun.auto-route",
    "tun.auto-detect-interface",
    "tun.strict-route",
    "tun.dns-hijack",
];

/// Configuration keys that require a full effective-configuration reload (`PUT /configs`)
/// rather than an in-place partial patch.
///
/// Strictly disjoint from [`HOT_UPDATE_CONFIG_KEYS`].
pub const FULL_RELOAD_CONFIG_KEYS: &[&str] = &[
    "dns",
    "sniffer",
    "hosts",
    "geox-url",
    "geodata-mode",
    "geodata-loader",
    "geo-auto-update",
    "geo-update-interval",
    "profile",
    "external-controller",
    "secret",
    "proxies",
    "proxy-groups",
    "proxy-providers",
    "rules",
    "rule-providers",
    "tun.mtu",
    "tun.inet4-address",
    "tun.inet6-address",
    "tun.endpoint-independent-nat",
];

/// Explicit dotted-path override key set distinguishing "inherit from profile"
/// (key absent from set) from "explicitly cleared" (key present in set with
/// `Value::Null` or missing value in the controlled patch).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverrideKeys {
    keys: BTreeSet<String>,
}

impl OverrideKeys {
    /// Creates an empty override key set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an override key set from dotted key paths (for example `"tun.mtu"` or
    /// `"dns.fallback-filter.geoip"`).
    #[must_use]
    pub fn from_paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let keys = paths
            .into_iter()
            .map(Into::into)
            .filter(|path| !path.trim().is_empty())
            .collect();
        Self { keys }
    }

    /// Inserts one dotted key path into the explicit override set.
    pub fn insert(&mut self, path: impl Into<String>) -> bool {
        let path = path.into();
        if path.trim().is_empty() {
            return false;
        }
        self.keys.insert(path)
    }

    /// Returns whether `path` is explicitly managed by the controlled layer.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.keys.contains(path)
    }

    /// Iterates over all explicitly managed dotted key paths in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(String::as_str)
    }

    /// Returns whether no explicit override keys are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Computes the SHA-256 content hash of a rendered YAML document for
/// unchanged-content short-circuiting before pushing a reload.
#[must_use]
pub fn yaml_content_hash(yaml: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(yaml.as_bytes());
    let digest = hasher.finalize();
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&digest);
    hash
}

/// Output of a single [`EffectiveConfigPipeline`] synthesis pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveConfigArtifact {
    /// Final validated and normalized YAML serialized once.
    pub yaml: Arc<str>,
    /// SHA-256 content hash of [`Self::yaml`] for unchanged-content short-circuiting.
    pub content_hash: [u8; 32],
    /// Session-only listener port fallbacks applied during this pass.
    pub fallbacks: Vec<ListenerPortFallback>,
}

/// Inputs to one [`EffectiveConfigPipeline`] synthesis pass.
#[derive(Clone, Debug)]
pub struct EffectiveConfigRequest {
    /// Raw source profile YAML payload.
    pub source_yaml: String,
    /// Controlled override document (mapping) applied over the source profile.
    pub controlled_override: Value,
    /// Optional explicit set of controlled key paths (supporting explicit clear when absent or `Value::Null`).
    pub override_keys: Option<OverrideKeys>,
    /// Ordered external YAML override file paths applied after the controlled layer.
    pub yaml_overrides: Vec<PathBuf>,
    /// Target runtime core kind for normalization defaults.
    pub core_kind: CoreKind,
    /// Whether runtime policy forces `tun.enable: false` (e.g., non-elevated Local launch).
    pub disable_tun_for_runtime: bool,
    /// Existing session listener fallbacks to re-apply to unchanged original ports.
    pub(crate) session_fallbacks: BTreeMap<String, SessionListenerFallback>,
    /// Whether to probe sockets and resolve occupied listener ports into session fallbacks.
    pub probe_listener_conflicts: bool,
}

/// Single-`Value`-stage effective configuration synthesis engine.
pub struct EffectiveConfigPipeline;

impl EffectiveConfigPipeline {
    /// Executes the complete effective-configuration pipeline in a single
    /// parse -> `Value` transform -> serialize pass.
    ///
    /// # Errors
    ///
    /// Returns an error if the source or any override exceeds size bounds, fails
    /// YAML parsing/merge expansion, fails validation, or fails listener fallback probing.
    pub fn compose(
        mut request: EffectiveConfigRequest,
    ) -> ControlledConfigResult<EffectiveConfigArtifact> {
        if request.source_yaml.len() > MAX_PROFILE_BYTES {
            return Err(ControlledConfigError::TooLarge);
        }

        // 1. Parse source once and expand YAML `<<` merge keys.
        let mut document: Value = serde_yaml::from_str(&request.source_yaml)?;
        expand_yaml_merges(&mut document)?;
        if !document.is_mapping() {
            return Err(ControlledConfigError::NotMapping);
        }

        // 2. Apply controlled override (supporting `Value::Null` key clearing & `OverrideKeys`).
        let mut controlled = request.controlled_override;
        expand_yaml_merges(&mut controlled)?;
        apply_controlled_override(&mut document, controlled, request.override_keys.as_ref());

        // 3. Apply ordered YAML overrides in place on `Value`.
        if !request.yaml_overrides.is_empty() {
            merge_value_overrides(&mut document, &request.yaml_overrides)?;
        }

        // 4. Normalize TUN defaults & GeoX URLs in place on `Value`.
        normalize_runtime_value(request.core_kind, &mut document)?;

        // 5. Project runtime policy & listener fallbacks in place on `Value`.
        if request.disable_tun_for_runtime {
            let mut tun = serde_yaml::Mapping::new();
            tun.insert(Value::String("enable".into()), Value::Bool(false));
            let mut patch = serde_yaml::Mapping::new();
            patch.insert(Value::String("tun".into()), Value::Mapping(tun));
            merge_controlled_yaml(&mut document, Value::Mapping(patch));
        }

        apply_session_fallbacks_to_value(&mut document, &request.session_fallbacks)
            .map_err(ControlledConfigError::ListenerFallback)?;

        let fallbacks = if request.probe_listener_conflicts {
            let resolved = resolve_conflicts(&mut document, &mut request.session_fallbacks)
                .map_err(ControlledConfigError::ListenerFallback)?;
            resolved
                .into_iter()
                .map(|(listener, fallback)| ListenerPortFallback {
                    listener,
                    original: fallback.original,
                    current: fallback.current,
                })
                .collect()
        } else {
            Vec::new()
        };

        // 6. Validate and serialize ONCE to final YAML string + SHA-256 content hash.
        let yaml = serialize_profile(&document)?;
        let content_hash = yaml_content_hash(&yaml);
        Ok(EffectiveConfigArtifact {
            yaml: Arc::from(yaml),
            content_hash,
            fallbacks,
        })
    }

    /// Runs [`Self::compose`] on a Tokio blocking worker so YAML parsing, merge
    /// expansion, and `socket2` port probing never block async worker threads.
    ///
    /// # Errors
    ///
    /// Returns pipeline errors or [`ControlledConfigError::Task`] if the blocking worker fails.
    pub async fn compose_async(
        request: EffectiveConfigRequest,
    ) -> ControlledConfigResult<EffectiveConfigArtifact> {
        tokio::task::spawn_blocking(move || Self::compose(request))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

/// Applies a controlled override mapping onto `target`, supporting:
/// - Complete replacement semantics for `hosts` and `dns.nameserver-policy`,
/// - Explicit `Value::Null` key clearing (removing keys such as `tun.mtu` or
///   `dns.fallback-filter.geoip` instead of leaving `null` or keeping profile defaults),
/// - Optional [`OverrideKeys`] explicit clearing when a selected dotted path is
///   absent or `Value::Null` in `controlled`.
pub fn apply_controlled_override(
    target: &mut Value,
    controlled: Value,
    override_keys: Option<&OverrideKeys>,
) {
    let hosts = controlled.get("hosts").cloned();
    let policy = controlled
        .get("dns")
        .and_then(|dns| dns.get("nameserver-policy"))
        .cloned();

    merge_with_null_clearing(target, &controlled);

    if let Some(hosts_value) = hosts {
        if hosts_value.is_null() {
            remove_dotted_key(target, "hosts");
        } else if let Some(mapping) = target.as_mapping_mut() {
            mapping.insert(Value::String("hosts".into()), hosts_value);
        }
    }
    if let Some(policy_value) = policy {
        if policy_value.is_null() {
            remove_dotted_key(target, "dns.nameserver-policy");
        } else if let Some(dns) = target.get_mut("dns").and_then(Value::as_mapping_mut) {
            dns.insert(Value::String("nameserver-policy".into()), policy_value);
        }
    }

    if let Some(override_keys) = override_keys {
        for dotted in override_keys.iter() {
            let value = lookup_dotted_key(&controlled, dotted);
            if value.is_none_or(Value::is_null) {
                remove_dotted_key(target, dotted);
            }
        }
    }
}

fn merge_with_null_clearing(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Mapping(target_map), Value::Mapping(patch_map)) => {
            for (key, value) in patch_map {
                if value.is_null() {
                    target_map.remove(key);
                    continue;
                }
                match target_map.get_mut(key) {
                    Some(existing) if existing.is_mapping() && value.is_mapping() => {
                        merge_with_null_clearing(existing, value);
                    }
                    _ => {
                        let mut cloned = value.clone();
                        strip_null_entries(&mut cloned);
                        target_map.insert(key.clone(), cloned);
                    }
                }
            }
        }
        (target_val, patch_val) if !patch_val.is_null() => {
            let mut cloned = patch_val.clone();
            strip_null_entries(&mut cloned);
            *target_val = cloned;
        }
        _ => {}
    }
}

fn strip_null_entries(value: &mut Value) {
    if let Value::Mapping(mapping) = value {
        let null_keys: Vec<Value> = mapping
            .iter()
            .filter(|(_, val)| val.is_null())
            .map(|(key, _)| key.clone())
            .collect();
        for key in null_keys {
            mapping.remove(&key);
        }
        for child in mapping.values_mut() {
            strip_null_entries(child);
        }
    }
}

fn lookup_dotted_key<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in dotted.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn remove_dotted_key(value: &mut Value, dotted: &str) {
    let (parents, last) = match dotted.rsplit_once('.') {
        Some((parents, last)) => (Some(parents), last),
        None => (None, dotted),
    };
    let mut current = value;
    if let Some(parents) = parents {
        for segment in parents.split('.') {
            let Some(next) = current.get_mut(segment) else {
                return;
            };
            current = next;
        }
    }
    if let Some(mapping) = current.as_mapping_mut() {
        mapping.remove(Value::String(last.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn hot_update_and_full_reload_config_keys_are_strictly_disjoint() {
        let hot: HashSet<&str> = HOT_UPDATE_CONFIG_KEYS.iter().copied().collect();
        let full: HashSet<&str> = FULL_RELOAD_CONFIG_KEYS.iter().copied().collect();
        assert_eq!(hot.len(), HOT_UPDATE_CONFIG_KEYS.len());
        assert_eq!(full.len(), FULL_RELOAD_CONFIG_KEYS.len());
        let overlap: Vec<&str> = hot.intersection(&full).copied().collect();
        assert!(
            overlap.is_empty(),
            "HOT_UPDATE_CONFIG_KEYS and FULL_RELOAD_CONFIG_KEYS must not overlap: {overlap:?}"
        );
    }

    #[test]
    fn pipeline_supports_explicit_null_key_clearing_and_override_keys() {
        let source = r#"
mixed-port: 7890
tun:
  enable: false
  mtu: 9000
dns:
  enable: true
  fallback-filter:
    geoip: true
    geoip-code: CN
rules:
  - MATCH,DIRECT
"#;
        let controlled: Value = serde_yaml::from_str(
            r#"
tun:
  mtu: null
dns:
  fallback-filter:
    geoip: null
"#,
        )
        .unwrap();
        let override_keys = OverrideKeys::from_paths(["tun.mtu", "dns.fallback-filter.geoip"]);

        let artifact = EffectiveConfigPipeline::compose(EffectiveConfigRequest {
            source_yaml: source.to_owned(),
            controlled_override: controlled,
            override_keys: Some(override_keys),
            yaml_overrides: Vec::new(),
            core_kind: CoreKind::Mihomo,
            disable_tun_for_runtime: false,
            session_fallbacks: BTreeMap::new(),
            probe_listener_conflicts: false,
        })
        .unwrap();

        let parsed: Value = serde_yaml::from_str(&artifact.yaml).unwrap();
        assert!(parsed["tun"].get("mtu").is_none());
        assert!(parsed["dns"]["fallback-filter"].get("geoip").is_none());
        assert_eq!(
            parsed["dns"]["fallback-filter"]["geoip-code"].as_str(),
            Some("CN")
        );
        assert!(parsed["geox-url"]["geoip"].as_str().is_some());
        assert_eq!(artifact.content_hash, yaml_content_hash(&artifact.yaml));
    }

    #[test]
    fn pipeline_content_hash_detects_unchanged_and_changed_output() {
        let req = |mode: &str| EffectiveConfigRequest {
            source_yaml: format!("mixed-port: 7890\nmode: {mode}\nrules:\n  - MATCH,DIRECT\n"),
            controlled_override: Value::Mapping(serde_yaml::Mapping::new()),
            override_keys: None,
            yaml_overrides: Vec::new(),
            core_kind: CoreKind::Mihomo,
            disable_tun_for_runtime: false,
            session_fallbacks: BTreeMap::new(),
            probe_listener_conflicts: false,
        };

        let first = EffectiveConfigPipeline::compose(req("rule")).unwrap();
        let second = EffectiveConfigPipeline::compose(req("rule")).unwrap();
        let third = EffectiveConfigPipeline::compose(req("global")).unwrap();

        assert_eq!(first.content_hash, second.content_hash);
        assert_ne!(first.content_hash, third.content_hash);
    }

    #[test]
    fn pipeline_expands_yaml_merges_and_projects_local_tun_disabled_in_single_pass() {
        let source = r#"
defaults: &tun_defaults
  enable: true
  stack: system
mixed-port: 7890
tun:
  <<: *tun_defaults
rules:
  - MATCH,DIRECT
"#;
        let artifact = EffectiveConfigPipeline::compose(EffectiveConfigRequest {
            source_yaml: source.to_owned(),
            controlled_override: Value::Mapping(serde_yaml::Mapping::new()),
            override_keys: None,
            yaml_overrides: Vec::new(),
            core_kind: CoreKind::Mihomo,
            disable_tun_for_runtime: true,
            session_fallbacks: BTreeMap::new(),
            probe_listener_conflicts: false,
        })
        .unwrap();

        let parsed: Value = serde_yaml::from_str(&artifact.yaml).unwrap();
        assert_eq!(parsed["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(parsed["tun"]["stack"].as_str(), Some("system"));
    }
}
