//! Core-specific compatibility policy for partial TUN payload updates.

use super::{ControlledConfigError, ControlledConfigResult};
use crate::profiles::MAX_PROFILE_BYTES;
use serde_yaml::Value;

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
