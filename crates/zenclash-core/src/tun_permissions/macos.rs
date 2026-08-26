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
    let script = concat!(
        "on run argv\n",
        "set corePath to quoted form of item 1 of argv\n",
        "do shell script \"/usr/sbin/chown root:admin -- \" & corePath & ",
        " \" && /bin/chmod u+s -- \" & corePath with administrator privileges\n",
        "end run"
    );
    let path = binary
        .to_str()
        .ok_or_else(|| TunPermissionError::InvalidBinary("macOS 内核路径不是有效 UTF-8".into()))?;
    let output = platform_command::output_with_timeout(
        "/usr/bin/osascript",
        &["-e", script, path],
        AUTHORIZATION_TIMEOUT,
    )
    .map_err(TunPermissionError::Platform)?;
    if !output.status.success() {
        return Err(TunPermissionError::Platform(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let verified = status(binary)?;
    if !verified.granted {
        return Err(TunPermissionError::Verification(verified.detail));
    }
    Ok(TunPermissionGrant::Ready(verified))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_test_binary_reports_real_permission_bits() {
        let binary = std::env::current_exe().unwrap();
        let status = status(&binary).unwrap();

        assert!(!status.granted);
        assert!(status.can_request);
        assert_eq!(status.binary, binary);
    }
}
