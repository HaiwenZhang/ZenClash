//! Current-user WinINET registry access; no proxy mutation ordering lives here.
use super::connection_transaction::DefaultConnectionAccess;
use super::*;
use crate::MihomoError;
use crate::system_proxy::native::unstable_observation;
use std::ptr;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
    System::Registry::*,
};

const INTERNET_SETTINGS_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

pub(super) struct InternetSettingsKey(HKEY);
impl Drop for InternetSettingsKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}
fn error(operation: &'static str, code: u32) -> MihomoError {
    crate::SystemProxyError::Platform {
        backend: crate::SystemProxyBackendKind::WinInet,
        operation,
        code: i64::from(code),
    }
    .into()
}
impl InternetSettingsKey {
    pub(super) fn open(writing: bool) -> MihomoResult<Self> {
        let path = utf16_units(INTERNET_SETTINGS_PATH)
            .into_iter()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut key = ptr::null_mut();
        // SAFETY: NUL-terminated path and live output; only current-user settings.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                KEY_QUERY_VALUE | if writing { KEY_SET_VALUE } else { 0 },
                &raw mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(error("RegOpenKeyExW", status));
        }
        Ok(Self(key))
    }
    fn read_values(&self) -> MihomoResult<InternetSettingsRegistryValues> {
        let mut values = InternetSettingsRegistryValues::new();
        let mut budget = MAX_CAPTURE_BYTES;
        for index in 0..65_536 {
            let mut name = vec![0_u16; 16_384];
            let mut name_length = name.len() as u32;
            let mut kind = 0;
            let mut length = 0;
            // SAFETY: Sized name buffer and outputs. Null data requests its size.
            let status = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &raw mut name_length,
                    ptr::null(),
                    &raw mut kind,
                    ptr::null_mut(),
                    &raw mut length,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                return Ok(values);
            }
            if status != ERROR_SUCCESS {
                return Err(error("RegEnumValueW", status));
            }
            name.truncate(name_length as usize);
            let name_bytes = name
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .ok_or_else(invalid_snapshot)?;
            budget = budget
                .checked_sub(name_bytes)
                .ok_or_else(invalid_snapshot)?;
            let nul_name = name
                .iter()
                .copied()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let mut bytes = Vec::new();
            let mut completed = false;
            for _ in 0..8 {
                if length as usize > budget {
                    return Err(invalid_snapshot());
                }
                bytes.resize(length as usize, 0);
                // SAFETY: Bounded byte buffer, valid key, NUL-terminated name.
                let status = unsafe {
                    RegQueryValueExW(
                        self.0,
                        nul_name.as_ptr(),
                        ptr::null(),
                        &raw mut kind,
                        bytes.as_mut_ptr(),
                        &raw mut length,
                    )
                };
                if status == ERROR_MORE_DATA {
                    continue;
                }
                if status != ERROR_SUCCESS {
                    return Err(error("RegQueryValueExW", status));
                }
                bytes.truncate(length as usize);
                completed = true;
                break;
            }
            if !completed {
                return Err(unstable_observation());
            }
            budget -= bytes.len();
            values.insert(
                ascii_folded_value_name(&name),
                RawRegistryValue {
                    name,
                    kind,
                    bytes: bytes.into(),
                },
            );
        }
        Err(invalid_snapshot())
    }
    fn write_value(&self, name: &str, value: Option<&RawRegistryValue>) -> MihomoResult<()> {
        let name = value
            .map_or_else(|| utf16_units(name), |value| value.name.clone())
            .into_iter()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: Captured typed data and sized slice remain live for the call.
        let status = unsafe {
            if let Some(value) = value {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                )
            } else {
                RegDeleteValueW(self.0, name.as_ptr())
            }
        };
        if status != ERROR_SUCCESS && !(value.is_none() && status == ERROR_FILE_NOT_FOUND) {
            return Err(error("write WinINET value", status));
        }
        Ok(())
    }
}

impl DefaultConnectionAccess for InternetSettingsKey {
    fn read_values(&self) -> MihomoResult<InternetSettingsRegistryValues> {
        self.read_values()
    }
    fn query_connection(&self) -> MihomoResult<DefaultConnectionOptions> {
        crate::system_proxy::windows::connection_options::query_default()
    }
    fn set_option(&self, option: &DefaultConnectionOption) -> MihomoResult<()> {
        crate::system_proxy::windows::connection_options::set_default_option(option)
    }
    fn restore_registry_value(
        &self,
        name: &str,
        value: Option<&RawRegistryValue>,
    ) -> MihomoResult<()> {
        self.write_value(name, value)
    }
    fn refresh(&self) -> MihomoResult<()> {
        super::super::notify_wininet()
    }
}
