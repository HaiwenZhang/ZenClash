use std::path::Path;

use crate::{TunPermissionError, TunPermissionResult, TunPermissionStatus, platform_command};

pub(super) fn status(binary: &Path) -> TunPermissionResult<TunPermissionStatus> {
    let output = platform_command::output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
        ],
    )
    .map_err(TunPermissionError::Platform)?;
    if !output.status.success() {
        return Err(TunPermissionError::Platform(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let granted = String::from_utf8_lossy(&output.stdout)
        .trim()
        .eq_ignore_ascii_case("true");
    Ok(TunPermissionStatus {
        granted,
        can_request: false,
        binary: binary.to_path_buf(),
        detail: if granted {
            "ZenClash 当前以管理员权限运行".into()
        } else {
            "ZenClash 当前不是管理员进程；尚未提供具备调用方 ACL 的 Windows TUN helper，拒绝提升整个 GUI".into()
        },
    })
}

pub(super) fn request_grant(_binary: &Path) -> TunPermissionResult<TunPermissionStatus> {
    Err(TunPermissionError::Unsupported(
        "Windows 尚无具备调用方 ACL 的按需 TUN helper；不会提升整个 ZenClash GUI".into(),
    ))
}
