use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};

use crate::{
    TunPermissionError, TunPermissionResult, TunPermissionStatus, platform_command,
    tun_permissions::binary_sha256,
};

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);
const AUTHORIZATION_SCRIPT: &str = concat!(
    "on run argv\n",
    "set corePath to quoted form of item 1 of argv\n",
    "set expectedHash to quoted form of item 2 of argv\n",
    "set commandText to \"set -eu; /usr/sbin/chown root:admin \" & corePath & ",
    "\"; actual=$(/usr/bin/shasum -a 256 -- \" & corePath & \" | /usr/bin/awk '{print $1}'); \" & ",
    "\"if [ \\\"$actual\\\" != \" & expectedHash & \" ]; then exit 65; fi; \" & ",
    "\"/bin/chmod 4755 \" & corePath & \"; verified=$(/usr/bin/shasum -a 256 -- \" & corePath & ",
    "\" | /usr/bin/awk '{print $1}'); if [ \\\"$verified\\\" != \" & expectedHash & ",
    "\" ]; then /bin/chmod u-s \" & corePath & \"; exit 66; fi\"\n",
    "do shell script commandText with administrator privileges\n",
    "end run"
);

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
        .ok_or_else(|| TunPermissionError::InvalidBinary("macOS 内核路径不是有效 UTF-8".into()))?;
    let expected = binary_sha256(binary)?;
    let output = platform_command::output_with_timeout(
        "/usr/bin/osascript",
        &["-e", AUTHORIZATION_SCRIPT, path, &expected],
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
    Ok(verified)
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

    #[test]
    fn authorization_applescript_compiles_before_any_privilege_prompt() {
        let output = std::env::temp_dir().join(format!(
            "zenclash-tun-authorization-{}-{}.scpt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let compiled = std::process::Command::new("/usr/bin/osacompile")
            .args(["-e", AUTHORIZATION_SCRIPT, "-o"])
            .arg(&output)
            .output()
            .unwrap();

        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn authorization_script_uses_macos_compatible_chown_and_chmod_arguments() {
        let unsupported_arguments = [
            "/usr/sbin/chown root:admin -- ",
            "/bin/chmod 4755 -- ",
            "/bin/chmod u-s -- ",
        ];

        assert!(
            unsupported_arguments
                .iter()
                .all(|arguments| !AUTHORIZATION_SCRIPT.contains(arguments)),
            "{AUTHORIZATION_SCRIPT}"
        );
    }
}
