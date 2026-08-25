use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::{profiles::read_profile_bytes, MihomoError, MihomoResult};

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
    serde_yaml::to_string(&document)
        .map_err(|error| MihomoError::Process(format!("无法序列化合并后的 Mihomo 配置：{error}")))
}

fn read_yaml(path: &Path, kind: &str) -> MihomoResult<Value> {
    let payload = read_profile_bytes(path).map_err(|error| {
        MihomoError::Process(format!("无法读取{kind} {}：{error}", path.display()))
    })?;
    let source = String::from_utf8(payload).map_err(|error| {
        MihomoError::Process(format!("{kind} {} 不是 UTF-8：{error}", path.display()))
    })?;
    serde_yaml::from_str(&source).map_err(|error| {
        MihomoError::Process(format!("无法解析{kind} {}：{error}", path.display()))
    })
}

fn merge_yaml(target: &mut Value, patch: Value) {
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
}
