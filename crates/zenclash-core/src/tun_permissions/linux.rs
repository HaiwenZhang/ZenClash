use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};

use crate::{
    platform_command, TunPermissionError, TunPermissionGrant, TunPermissionResult,
    TunPermissionStatus,
};

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) fn status(binary: &Path) -> TunPermissionResult<TunPermissionStatus> {
    let metadata = binary.metadata().map_err(|error| {
        TunPermissionError::Platform(format!("无法读取 {}：{error}", binary.display()))
    })?;
    let root_owned = metadata.uid() == 0;
    let setuid = metadata.mode() & 0o4000 != 0;
    Ok(TunPermissionStatus {
        granted: root_owned && setuid,
        can_request: true,
        requires_relaunch: false,
        binary: binary.to_path_buf(),
        detail: format!(
            "所有者 UID {} · setuid {}",
            metadata.uid(),
            if setuid { "已设置" } else { "未设置" }
        ),
    })
}

pub(super) fn request_grant(binary: &Path) -> TunPermissionResult<TunPermissionGrant> {
    let path = binary
        .to_str()
        .ok_or_else(|| TunPermissionError::InvalidBinary("Linux 内核路径不是有效 UTF-8".into()))?;
    run_pkexec(&["chown", "root:root", path])?;
    run_pkexec(&["chmod", "u+s", path])?;
    let verified = status(binary)?;
    if !verified.granted {
        return Err(TunPermissionError::Verification(verified.detail));
    }
    Ok(TunPermissionGrant::Ready(verified))
}

fn run_pkexec(args: &[&str]) -> TunPermissionResult<()> {
    let output = platform_command::output_with_timeout("pkexec", args, AUTHORIZATION_TIMEOUT)
        .map_err(TunPermissionError::Platform)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TunPermissionError::Platform(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}
