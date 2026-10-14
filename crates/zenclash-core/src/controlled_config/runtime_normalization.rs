//! Core-specific compatibility policy for generated runtime configuration.

use super::{ControlledConfigError, ControlledConfigResult};
use crate::{CoreKind, profiles::MAX_PROFILE_BYTES};
use serde_yaml::Value;

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

pub(super) fn normalize_partial_tun_payload(payload: String) -> ControlledConfigResult<String> {
    let mut document = serde_yaml::from_str::<Value>(&payload)?;
    let root = document
        .as_mapping_mut()
        .ok_or(ControlledConfigError::NotMapping)?;
    if !crate::tun_config::normalize(root) {
        return Ok(payload);
    }
    let normalized = serde_yaml::to_string(&document)?;
    if normalized.len() > MAX_PROFILE_BYTES {
        return Err(ControlledConfigError::TooLarge);
    }
    Ok(normalized)
}

pub(crate) fn normalize_runtime_payload(
    _kind: CoreKind,
    payload: String,
) -> ControlledConfigResult<String> {
    let mut document = serde_yaml::from_str::<Value>(&payload)?;
    let root = document
        .as_mapping_mut()
        .ok_or(ControlledConfigError::NotMapping)?;
    let tun_changed = crate::tun_config::normalize(root);
    if !insert_missing_mihomo_geox_urls(root) && !tun_changed {
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
