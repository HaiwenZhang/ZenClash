//! Raw Internet Settings value types and exact byte representations.

use std::collections::BTreeMap;

use crate::{MihomoResult, system_proxy::native::invalid_snapshot};

pub(super) const REG_SZ_KIND: u32 = 1;
pub(super) const REG_EXPAND_SZ_KIND: u32 = 2;
pub(super) const REG_DWORD_KIND: u32 = 4;
pub(super) const MANAGED_VALUES: [&str; 4] = [
    "ProxyServer",
    "ProxyOverride",
    "AutoConfigURL",
    "ProxyEnable",
];

#[derive(Clone, PartialEq, Eq)]
pub(super) struct RawRegistryValue {
    pub(super) name: Vec<u16>,
    pub(super) kind: u32,
    pub(super) bytes: std::sync::Arc<[u8]>,
}
pub(super) type InternetSettingsRegistryValues = BTreeMap<Vec<u16>, RawRegistryValue>;

pub(super) fn ascii_folded_value_name(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&value| {
            if (65..=90).contains(&value) {
                value + 32
            } else {
                value
            }
        })
        .collect()
}
pub(super) fn utf16_units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}
pub(super) fn registry_string_value(name: &str, value: &str) -> RawRegistryValue {
    let bytes = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    RawRegistryValue {
        name: utf16_units(name),
        kind: REG_SZ_KIND,
        bytes: bytes.into(),
    }
}
pub(super) fn proxy_enable_value(enabled: bool) -> RawRegistryValue {
    RawRegistryValue {
        name: utf16_units("ProxyEnable"),
        kind: REG_DWORD_KIND,
        bytes: u32::from(enabled).to_le_bytes().to_vec().into(),
    }
}
pub(super) fn registry_string(
    values: &InternetSettingsRegistryValues,
    name: &str,
) -> MihomoResult<String> {
    let Some(value) = values.get(&ascii_folded_value_name(&utf16_units(name))) else {
        return Ok(String::new());
    };
    if !matches!(value.kind, REG_SZ_KIND | REG_EXPAND_SZ_KIND) || value.bytes.len() % 2 != 0 {
        return Err(invalid_snapshot());
    }
    let units = value
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| invalid_snapshot())
}
pub(super) fn registry_manual_proxy_enabled(
    values: &InternetSettingsRegistryValues,
) -> MihomoResult<bool> {
    let Some(value) = values.get(&ascii_folded_value_name(&utf16_units("ProxyEnable"))) else {
        return Ok(false);
    };
    if value.kind != REG_DWORD_KIND || value.bytes.len() != 4 {
        return Err(invalid_snapshot());
    }
    Ok(value.bytes.as_ref() != [0, 0, 0, 0])
}
