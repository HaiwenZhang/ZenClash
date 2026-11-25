use std::path::Path;

#[cfg(target_os = "windows")]
use super::{AutostartError, AutostartResult, AutostartStatus};

#[cfg(target_os = "windows")]
const TASK_NAME: &str = "ZenClash";
#[cfg(target_os = "windows")]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_KEY_DISPLAY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const STARTUP_APPROVED_RUN_SUBKEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

#[cfg(target_os = "windows")]
pub(super) fn default_entry_path() -> AutostartResult<Option<std::path::PathBuf>> {
    Ok(None)
}

#[cfg(target_os = "windows")]
pub(super) fn status(
    executable: &Path,
    _entry_path: Option<&Path>,
) -> AutostartResult<AutostartStatus> {
    let expected = quoted_executable(executable);
    let task = command_output("schtasks.exe", &["/Query", "/TN", TASK_NAME, "/XML"])?;
    if task.status.success() {
        let output = String::from_utf8_lossy(&task.stdout);
        return Ok(AutostartStatus {
            enabled: true,
            matches_current_executable: output
                .contains(&xml_escape(&executable.display().to_string())),
            location: format!("Windows Task Scheduler / {TASK_NAME}"),
        });
    }

    let registered = registry::read_run_value(TASK_NAME)?;
    let approved_disabled = registry::read_startup_approved_disabled(TASK_NAME)?;
    let enabled = registered.is_some() && !approved_disabled;
    let matches_current_executable = enabled
        && registered
            .as_deref()
            .is_some_and(|value| value.trim() == expected);
    Ok(AutostartStatus {
        enabled,
        matches_current_executable,
        location: format!("{RUN_KEY_DISPLAY} / {TASK_NAME}"),
    })
}

#[cfg(target_os = "windows")]
pub(super) fn set_enabled(
    executable: &Path,
    _entry_path: Option<&Path>,
    enabled: bool,
) -> AutostartResult<()> {
    if !enabled {
        let _ = command_output("schtasks.exe", &["/Delete", "/TN", TASK_NAME, "/F"])?;
        registry::delete_run_value(TASK_NAME)?;
        return Ok(());
    }

    let quoted = quoted_executable(executable);
    registry::delete_run_value(TASK_NAME)?;
    let task = command_output(
        "schtasks.exe",
        &[
            "/Create", "/TN", TASK_NAME, "/SC", "ONLOGON", "/TR", &quoted, "/F", "/RL", "LIMITED",
        ],
    )?;
    if task.status.success() {
        return Ok(());
    }

    registry::write_run_value(TASK_NAME, &quoted)?;
    // Clear any stale Task Manager disabled flag so re-enabling takes effect.
    let _ = registry::delete_startup_approved_value(TASK_NAME);
    Ok(())
}

#[cfg(target_os = "windows")]
fn command_output(command: &str, args: &[&str]) -> AutostartResult<std::process::Output> {
    crate::platform::command::output(command, args).map_err(AutostartError::Command)
}

/// Decodes a Windows `Explorer\StartupApproved\Run` `REG_BINARY` value and
/// reports whether the user disabled the startup entry in Task Manager / Settings.
///
/// Windows writes a 12-byte structure whose first byte carries the status flags:
/// even values (`0x02`, `0x06`, `0x00`) indicate enabled/default, whereas odd
/// values (`0x03`, `0x01`, `0x07`, i.e. low bit set) indicate disabled.
pub(crate) fn is_startup_approved_disabled(data: &[u8]) -> bool {
    let Some(&flag) = data.first() else {
        return false;
    };
    (flag & 0x01) != 0
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn utf16_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_reg_sz_wide(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    while units.last() == Some(&0) {
        units.pop();
    }
    Some(String::from_utf16_lossy(&units))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn quoted_executable(executable: &Path) -> String {
    format!("\"{}\"", executable.display())
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_os = "windows")]
mod registry {
    use std::ptr;

    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_EXPAND_SZ,
            REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        },
    };

    use super::{
        AutostartError, AutostartResult, RUN_SUBKEY, STARTUP_APPROVED_RUN_SUBKEY,
        decode_reg_sz_wide, is_startup_approved_disabled, utf16_nul,
    };

    const MAX_REGISTRY_VALUE_BYTES: usize = 64 * 1024;

    struct RegKeyGuard(HKEY);

    impl Drop for RegKeyGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid key handle returned by `RegOpenKeyExW`.
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn open_hkcu(subkey: &str, access: u32) -> AutostartResult<Option<RegKeyGuard>> {
        let wide_subkey = utf16_nul(subkey);
        let mut handle: HKEY = ptr::null_mut();
        // SAFETY: `wide_subkey` is NUL-terminated UTF-16 and `handle` is a valid out pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide_subkey.as_ptr(),
                0,
                access,
                &raw mut handle,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(AutostartError::Command(format!(
                "RegOpenKeyExW({subkey}) failed with Windows status {status}"
            )));
        }
        Ok(Some(RegKeyGuard(handle)))
    }

    fn query_value(key: &RegKeyGuard, value_name: &str) -> AutostartResult<Option<(u32, Vec<u8>)>> {
        let wide_name = utf16_nul(value_name);
        let mut kind: u32 = 0;
        let mut length: u32 = 0;
        // SAFETY: Valid key handle, NUL-terminated name, null buffer queries required byte length.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                wide_name.as_ptr(),
                ptr::null(),
                &raw mut kind,
                ptr::null_mut(),
                &raw mut length,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(AutostartError::Command(format!(
                "RegQueryValueExW({value_name}) size query failed with Windows status {status}"
            )));
        }
        let mut buffer = Vec::new();
        for _ in 0..4 {
            let size = usize::try_from(length).unwrap_or(usize::MAX);
            if size > MAX_REGISTRY_VALUE_BYTES {
                return Err(AutostartError::Command(format!(
                    "Registry value {value_name} exceeds {MAX_REGISTRY_VALUE_BYTES} bytes"
                )));
            }
            buffer.resize(size, 0);
            // SAFETY: Valid key handle, NUL-terminated name, `buffer` has at least `length` bytes.
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    wide_name.as_ptr(),
                    ptr::null(),
                    &raw mut kind,
                    buffer.as_mut_ptr(),
                    &raw mut length,
                )
            };
            if status == ERROR_MORE_DATA {
                continue;
            }
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(AutostartError::Command(format!(
                    "RegQueryValueExW({value_name}) failed with Windows status {status}"
                )));
            }
            buffer.truncate(usize::try_from(length).unwrap_or(buffer.len()));
            return Ok(Some((kind, buffer)));
        }
        Err(AutostartError::Command(format!(
            "RegQueryValueExW({value_name}) changed concurrently"
        )))
    }

    pub(super) fn read_run_value(name: &str) -> AutostartResult<Option<String>> {
        let Some(key) = open_hkcu(RUN_SUBKEY, KEY_QUERY_VALUE)? else {
            return Ok(None);
        };
        let Some((kind, bytes)) = query_value(&key, name)? else {
            return Ok(None);
        };
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return Ok(None);
        }
        Ok(decode_reg_sz_wide(&bytes))
    }

    pub(super) fn read_startup_approved_disabled(name: &str) -> AutostartResult<bool> {
        let Some(key) = open_hkcu(STARTUP_APPROVED_RUN_SUBKEY, KEY_QUERY_VALUE)? else {
            return Ok(false);
        };
        let Some((kind, bytes)) = query_value(&key, name)? else {
            return Ok(false);
        };
        if kind != REG_BINARY {
            return Ok(false);
        }
        Ok(is_startup_approved_disabled(&bytes))
    }

    pub(super) fn write_run_value(name: &str, value: &str) -> AutostartResult<()> {
        let Some(key) = open_hkcu(RUN_SUBKEY, KEY_SET_VALUE)? else {
            return Err(AutostartError::Command(format!(
                "HKCU\\{RUN_SUBKEY} does not exist"
            )));
        };
        let wide_name = utf16_nul(name);
        let wide_value = utf16_nul(value);
        let byte_len =
            u32::try_from(wide_value.len().saturating_mul(std::mem::size_of::<u16>()))
                .map_err(|_| AutostartError::Command("Registry value length overflow".into()))?;
        // SAFETY: Valid key handle, NUL-terminated UTF-16 name and value buffers live across call.
        let status = unsafe {
            RegSetValueExW(
                key.0,
                wide_name.as_ptr(),
                0,
                REG_SZ,
                wide_value.as_ptr().cast::<u8>(),
                byte_len,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(AutostartError::Command(format!(
                "RegSetValueExW({name}) failed with Windows status {status}"
            )));
        }
        Ok(())
    }

    pub(super) fn delete_run_value(name: &str) -> AutostartResult<()> {
        delete_value(RUN_SUBKEY, name)
    }

    pub(super) fn delete_startup_approved_value(name: &str) -> AutostartResult<()> {
        delete_value(STARTUP_APPROVED_RUN_SUBKEY, name)
    }

    fn delete_value(subkey: &str, name: &str) -> AutostartResult<()> {
        let Some(key) = open_hkcu(subkey, KEY_SET_VALUE)? else {
            return Ok(());
        };
        let wide_name = utf16_nul(name);
        // SAFETY: Valid key handle and NUL-terminated UTF-16 value name.
        let status = unsafe { RegDeleteValueW(key.0, wide_name.as_ptr()) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(AutostartError::Command(format!(
                "RegDeleteValueW({name}) failed with Windows status {status}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        decode_reg_sz_wide, is_startup_approved_disabled, quoted_executable, utf16_nul, xml_escape,
    };

    #[test]
    fn task_action_quotes_an_executable_with_spaces() {
        assert_eq!(
            quoted_executable(Path::new(r"C:\Program Files\ZenClash\zenclash.exe")),
            r#""C:\Program Files\ZenClash\zenclash.exe""#
        );
    }

    #[test]
    fn task_query_match_escapes_xml_characters() {
        assert_eq!(
            xml_escape(r#""C:\A&B\zenclash.exe""#),
            r#""C:\A&amp;B\zenclash.exe""#
        );
    }

    #[test]
    fn startup_approved_decoding_detects_task_manager_disabled_entries() {
        // Windows enabled markers: 0x02 or 0x06 followed by zeroed timestamp bytes.
        assert!(!is_startup_approved_disabled(&[
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
        ]));
        assert!(!is_startup_approved_disabled(&[
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
        ]));
        assert!(!is_startup_approved_disabled(&[]));

        // Windows disabled markers: 0x03 or 0x01 / 0x07 (low bit set) + FILETIME timestamp.
        assert!(is_startup_approved_disabled(&[
            0x03, 0x00, 0x00, 0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x01
        ]));
        assert!(is_startup_approved_disabled(&[0x01, 0x00, 0x00, 0x00]));
        assert!(is_startup_approved_disabled(&[0x07, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn wide_reg_sz_roundtrip_preserves_unicode_windows_paths() {
        let original = r#""C:\Users\中文用户\AppData\Local\ZenClash\zenclash.exe""#;
        let units = utf16_nul(original);
        let bytes: Vec<u8> = units.into_iter().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_reg_sz_wide(&bytes).as_deref(), Some(original));
        assert!(decode_reg_sz_wide(&[0x41]).is_none());
    }
}
