use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};

use crate::{
    TunPermissionError, TunPermissionResult, TunPermissionStatus, platform_command,
    tun_permissions::binary_sha256,
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
        binary: binary.to_path_buf(),
        detail: format!(
            "所有者 UID {} · setuid {}",
            metadata.uid(),
            if setuid { "已设置" } else { "未设置" }
        ),
    })
}

pub(super) fn request_grant(binary: &Path) -> TunPermissionResult<TunPermissionStatus> {
    let path = binary
        .to_str()
        .ok_or_else(|| TunPermissionError::InvalidBinary("Linux 内核路径不是有效 UTF-8".into()))?;
    let expected = binary_sha256(binary)?;
    let script = concat!(
        "set -eu\n",
        "path=$1\n",
        "expected=$2\n",
        "chown root:root -- \"$path\"\n",
        "actual=$(sha256sum -- \"$path\")\n",
        "actual=${actual%% *}\n",
        "[ \"$actual\" = \"$expected\" ] || exit 65\n",
        "chmod 4755 -- \"$path\"\n",
        "verified=$(sha256sum -- \"$path\")\n",
        "verified=${verified%% *}\n",
        "if [ \"$verified\" != \"$expected\" ]; then chmod u-s -- \"$path\"; exit 66; fi\n"
    );
    run_pkexec(&["/bin/sh", "-c", script, "zenclash-tun", path, &expected])?;
    let verified = status(binary)?;
    if !verified.granted {
        return Err(TunPermissionError::Verification(verified.detail));
    }
    Ok(verified)
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
