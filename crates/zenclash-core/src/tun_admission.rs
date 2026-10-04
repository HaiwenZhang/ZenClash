//! Final payload policy for ordinary Mihomo owners; authorization belongs to Manager.

use serde::Deserialize;

use crate::{CoreKind, MihomoError, MihomoResult};

#[derive(Deserialize)]
struct TunPolicy {
    #[serde(default)]
    tun: Option<TunFlag>,
}

#[derive(Deserialize)]
struct TunFlag {
    #[serde(default)]
    enable: Option<bool>,
}

pub(crate) fn ensure_local_yaml(kind: CoreKind, payload: &str) -> MihomoResult<()> {
    if kind != CoreKind::Mihomo {
        return Ok(());
    }
    ensure_disabled(Some(yaml_enables_tun(payload)?))
}

pub(crate) fn yaml_enables_tun(payload: &str) -> MihomoResult<bool> {
    if payload.len() > crate::profiles::MAX_PROFILE_BYTES {
        return Err(MihomoError::InvalidInput(zenclash_i18n::text_with(
            "core_page.service.payload_too_large",
            &[(
                "limit",
                (crate::profiles::MAX_PROFILE_BYTES / (1024 * 1024)).to_string(),
            )],
        )));
    }
    let mut value: serde_yaml::Value = serde_yaml::from_str(payload)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
    crate::profile::expand_yaml_merges(&mut value)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
    let policy: TunPolicy = serde_yaml::from_value(value)
        .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
    Ok(policy.tun.and_then(|tun| tun.enable) == Some(true))
}

pub(crate) fn ensure_local_patch(kind: CoreKind, patch: &serde_json::Value) -> MihomoResult<()> {
    if kind != CoreKind::Mihomo {
        return Ok(());
    }
    // Go's JSON struct decoding accepts case variants. Inspect every matching
    // member so duplicate variants cannot hide a request to enable TUN.
    if let Some(fields) = patch.as_object() {
        for (name, tun) in fields {
            if !name.eq_ignore_ascii_case("tun") {
                continue;
            }
            if let Some(fields) = tun.as_object() {
                for (name, value) in fields {
                    if name.eq_ignore_ascii_case("enable") {
                        let enable = serde_json::from_value::<Option<bool>>(value.clone())
                            .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
                        ensure_disabled(enable)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn ensure_disabled(enable: Option<bool>) -> MihomoResult<()> {
    if enable == Some(true) {
        Err(MihomoError::ServiceRequired)
    } else {
        Ok(())
    }
}
