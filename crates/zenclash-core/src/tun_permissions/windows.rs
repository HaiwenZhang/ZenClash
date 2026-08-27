use std::path::Path;

use crate::{
    platform_command, TunPermissionError, TunPermissionGrant, TunPermissionResult,
    TunPermissionStatus,
};

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
        can_request: !granted,
        requires_relaunch: !granted,
        binary: binary.to_path_buf(),
        detail: if granted {
            "ZenClash 当前以管理员权限运行".into()
        } else {
            "ZenClash 当前不是管理员进程；Windows TUN 需要提权重启".into()
        },
    })
}

pub(super) fn request_grant(_binary: &Path) -> TunPermissionResult<TunPermissionGrant> {
    Ok(TunPermissionGrant::RelaunchRequested)
}
