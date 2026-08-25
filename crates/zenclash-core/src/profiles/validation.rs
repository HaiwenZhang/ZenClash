use std::time::{SystemTime, UNIX_EPOCH};

use super::{ProfileCatalog, ProfileStoreError, ProfileStoreResult, DEFAULT_USER_AGENT};

const MAX_PROFILE_ID_CHARS: usize = 64;

/// Validates that a payload is a non-empty Clash/Mihomo YAML mapping.
///
/// # Errors
///
/// Returns [`ProfileStoreError::InvalidYaml`] when parsing fails or no known
/// Clash/Mihomo top-level key is present.
pub fn validate_clash_yaml(payload: &str) -> ProfileStoreResult<()> {
    if payload.trim().is_empty() {
        return Err(ProfileStoreError::InvalidYaml("配置内容为空".into()));
    }
    let document: serde_yaml::Value = serde_yaml::from_str(payload)
        .map_err(|error| ProfileStoreError::InvalidYaml(error.to_string()))?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| ProfileStoreError::InvalidYaml("顶层必须是 YAML 映射".into()))?;
    let known_keys = [
        "proxies",
        "proxy-groups",
        "proxy-providers",
        "rules",
        "rule-providers",
        "mixed-port",
        "port",
        "socks-port",
        "dns",
        "tun",
    ];
    if !known_keys
        .iter()
        .any(|key| mapping.contains_key(serde_yaml::Value::String((*key).into())))
    {
        return Err(ProfileStoreError::InvalidYaml(
            "没有发现 Clash/Mihomo 配置字段".into(),
        ));
    }
    Ok(())
}

pub(super) fn normalized_user_agent(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        DEFAULT_USER_AGENT.into()
    } else {
        value.into()
    }
}

pub(super) fn unique_id(catalog: &ProfileCatalog, name: &str) -> String {
    let base = slug(name);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while catalog
        .profiles
        .iter()
        .any(|profile| profile.id == candidate)
    {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .take(MAX_PROFILE_ID_CHARS)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        return format!("profile-{}", unix_timestamp());
    }
    if is_windows_reserved_name(&slug) {
        format!("profile-{slug}")
    } else {
        slug
    }
}

fn is_windows_reserved_name(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    matches!(value.as_str(), "con" | "prn" | "aux" | "nul")
        || value
            .strip_prefix("com")
            .or_else(|| value.strip_prefix("lpt"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

pub(super) fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slug_prefixes_windows_reserved_file_names() {
        assert_eq!(slug("CON"), "profile-con");
    }

    #[test]
    fn slug_limits_profile_file_name_length() {
        assert_eq!(slug(&"a".repeat(100)).len(), 64);
    }
}
