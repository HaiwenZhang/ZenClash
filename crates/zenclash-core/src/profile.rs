use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::{
    MihomoError, MihomoResult,
    profiles::{MAX_PROFILE_BYTES, read_profile_bytes, validate_clash_yaml},
};

#[derive(Clone)]
pub(crate) enum ProfileRuntimeSource {
    File(PathBuf),
    Frozen(std::sync::Arc<str>),
    Prepared(Box<(crate::ControlledConfigUpdate, Option<String>)>),
}

impl From<PathBuf> for ProfileRuntimeSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

pub(crate) fn merge_payload_patch(payload: &str, patch: Value) -> MihomoResult<String> {
    let mut document = parse_payload(payload)?;
    merge_yaml(&mut document, patch);
    serialize_profile(&document)
}

/// Builds the effective Mihomo YAML without changing any source file.
/// Later override files win and mappings are merged recursively.
///
/// # Errors
///
/// Returns an error when any source is unreadable, exceeds the managed profile
/// size limit, is not UTF-8 or valid YAML, or the merged YAML cannot be encoded.
pub fn merge_profile_overrides(
    profile: impl AsRef<Path>,
    overrides: &[PathBuf],
) -> MihomoResult<String> {
    let profile = profile.as_ref();
    let mut document = read_yaml(profile, "基础配置")?;
    for path in overrides {
        let patch = read_yaml(path, "覆写")?;
        merge_yaml(&mut document, patch);
    }
    serialize_profile(&document)
}

pub fn merge_profile_patch(profile: &Path, patch: Value) -> MihomoResult<String> {
    let mut document = read_yaml(profile, "基础配置")?;
    merge_yaml(&mut document, patch);
    serialize_profile(&document)
}

pub fn merge_payload_overrides(payload: &str, overrides: &[PathBuf]) -> MihomoResult<String> {
    let mut document = parse_payload(payload)?;
    for path in overrides {
        let patch = read_yaml(path, "覆写")?;
        merge_yaml(&mut document, patch);
    }
    serialize_profile(&document)
}

fn parse_payload(payload: &str) -> MihomoResult<Value> {
    if payload.len() > MAX_PROFILE_BYTES {
        return Err(MihomoError::InvalidInput(format!(
            "基础配置超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )));
    }
    let mut document: Value = serde_yaml::from_str(payload)
        .map_err(|error| MihomoError::Process(format!("无法解析基础配置：{error}")))?;
    expand_yaml_merges(&mut document)
        .map_err(|error| MihomoError::Process(format!("无法解析基础配置：{error}")))?;
    Ok(document)
}

fn serialize_profile(document: &Value) -> MihomoResult<String> {
    let payload = serde_yaml::to_string(document).map_err(|error| {
        MihomoError::Process(format!("无法序列化合并后的 Mihomo 配置：{error}"))
    })?;
    if payload.len() > MAX_PROFILE_BYTES {
        return Err(MihomoError::InvalidInput(format!(
            "合并配置超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )));
    }
    validate_clash_yaml(&payload)
        .map_err(|error| MihomoError::Process(format!("合并后的 Mihomo 配置无效：{error}")))?;
    Ok(payload)
}

fn read_yaml(path: &Path, kind: &str) -> MihomoResult<Value> {
    let payload = read_profile_bytes(path).map_err(|error| {
        MihomoError::Process(format!("无法读取{kind} {}：{error}", path.display()))
    })?;
    let source = String::from_utf8(payload).map_err(|error| {
        MihomoError::Process(format!("{kind} {} 不是 UTF-8：{error}", path.display()))
    })?;
    let mut document: Value = serde_yaml::from_str(&source).map_err(|error| {
        MihomoError::Process(format!("无法解析{kind} {}：{error}", path.display()))
    })?;
    expand_yaml_merges(&mut document).map_err(|error| {
        MihomoError::Process(format!("无法解析{kind} {}：{error}", path.display()))
    })?;
    Ok(document)
}

pub(crate) fn expand_yaml_merges(document: &mut Value) -> Result<(), serde_yaml::Error> {
    fn expand(document: &mut Value, depth: usize) -> Result<(), serde_yaml::Error> {
        if depth > 128 {
            return Err(<serde_yaml::Error as serde::de::Error>::custom(
                zenclash_i18n::text("core_page.service.yaml_merge_depth"),
            ));
        }
        let has_merge = match document {
            Value::Mapping(mapping) => {
                for child in mapping.values_mut() {
                    expand(child, depth + 1)?;
                }
                mapping.contains_key(Value::from("<<"))
            }
            Value::Sequence(sequence) => {
                for child in sequence {
                    expand(child, depth + 1)?;
                }
                false
            }
            Value::Tagged(tagged) => {
                expand(&mut tagged.value, depth + 1)?;
                false
            }
            _ => false,
        };
        // Expand defaults before copying them; a single top-down apply_merge
        // can copy another merge key into a mapping it has already visited.
        if has_merge {
            document.apply_merge()?;
        }
        Ok(())
    }
    expand(document, 0)
}

pub fn merge_yaml(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Mapping(target), Value::Mapping(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(target_value) => merge_yaml(target_value, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::profiles::MAX_PROFILE_BYTES;

    #[test]
    fn merged_yaml_preserves_explicit_keys_and_first_sequence_default() {
        let source = "a: &a {enable: true, device: first}\nb: &b {<<: *a, enable: false}\ntun: {<<: [*b, *a]}\n";
        let payload = merge_payload_patch(source, Value::Mapping(Default::default())).unwrap();
        let result: Value = serde_yaml::from_str(&payload).unwrap();
        assert_eq!(result["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(result["tun"]["device"].as_str(), Some("first"));
        let payload = merge_payload_patch(
            "a: &a {enable: true}\nb: &b {<<: *a}\ntun: {<<: *b}\n",
            serde_yaml::to_value(serde_json::json!({"tun":{"enable":false}})).unwrap(),
        )
        .unwrap();
        let result: Value = serde_yaml::from_str(&payload).unwrap();
        assert_eq!(result["tun"]["enable"].as_bool(), Some(false));
    }

    #[test]
    fn merged_yaml_rejects_excessive_depth_and_invalid_defaults() {
        let mut document = Value::Null;
        for _ in 0..130 {
            document = Value::Sequence(vec![document]);
        }
        assert!(expand_yaml_merges(&mut document).is_err());
        assert!(merge_payload_patch("tun: {<<: 1}\n", Value::Mapping(Default::default())).is_err());
    }

    #[test]
    fn merged_override_enables_tun_after_expanding_its_own_defaults() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-merged-override-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("override.yaml");
        let source = "a: &a {enable: true}\nb: &b {<<: *a}\ntun: {<<: *b}\n";
        std::fs::write(&path, source).unwrap();
        let payload =
            merge_payload_overrides("tun: {enable: false}\n", std::slice::from_ref(&path)).unwrap();
        let result: Value = serde_yaml::from_str(&payload).unwrap();
        assert_eq!(result["tun"]["enable"].as_bool(), Some(true));
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recursively_merges_yaml_with_later_overrides_winning() {
        let mut base: Value = serde_yaml::from_str(
            "mode: rule\ndns:\n  enable: true\n  nameserver: [1.1.1.1]\nrules: ['MATCH,DIRECT']\n",
        )
        .unwrap();
        let patch: Value =
            serde_yaml::from_str("dns:\n  enable: false\n  ipv6: true\nrules: ['MATCH,Proxy']\n")
                .unwrap();
        merge_yaml(&mut base, patch);

        assert_eq!(base["mode"].as_str(), Some("rule"));
        assert_eq!(base["dns"]["enable"].as_bool(), Some(false));
        assert_eq!(base["dns"]["ipv6"].as_bool(), Some(true));
        assert_eq!(base["dns"]["nameserver"][0].as_str(), Some("1.1.1.1"));
        assert_eq!(base["rules"][0].as_str(), Some("MATCH,Proxy"));
    }

    #[test]
    fn read_yaml_rejects_files_above_the_profile_size_limit() {
        let sequence = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zenclash-oversized-override-{}-{sequence}.yaml",
            std::process::id()
        ));
        std::fs::write(&path, vec![b'a'; MAX_PROFILE_BYTES + 1]).unwrap();

        let error = read_yaml(&path, "覆写").unwrap_err();
        std::fs::remove_file(path).unwrap();

        assert!(error.to_string().contains("超过 16 MiB 限制"));
    }

    #[test]
    fn payload_overrides_preserve_controlled_values() {
        let root =
            std::env::temp_dir().join(format!("zenclash-payload-override-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let override_path = root.join("override.yaml");
        std::fs::write(&override_path, "dns:\n  ipv6: true\nmode: global\n").unwrap();

        let payload = merge_payload_overrides(
            "mixed-port: 7890\ndns:\n  enable: true\n  nameserver: [1.1.1.1]\nmode: rule\n",
            &[override_path],
        )
        .unwrap();
        let merged: Value = serde_yaml::from_str(&payload).unwrap();
        assert_eq!(merged["dns"]["enable"].as_bool(), Some(true));
        assert_eq!(merged["dns"]["ipv6"].as_bool(), Some(true));
        assert_eq!(merged["mode"].as_str(), Some("global"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
