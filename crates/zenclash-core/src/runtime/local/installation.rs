//! Runtime-core executable discovery and identity validation.

use std::{ffi::OsStr, path::PathBuf, time::Duration};

use thiserror::Error;

use crate::{CoreKind, platform::command as platform_command};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// A validated core executable that can run on the current machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreBinaryInfo {
    /// Core implementation proven by the executable's version output.
    pub kind: CoreKind,
    /// Canonical executable path.
    pub path: PathBuf,
    /// First non-empty line reported by the executable.
    pub version: String,
    /// Architecture of the currently running ZenClash process.
    pub architecture: &'static str,
}

/// Errors returned while validating a user-selected core executable.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreBinaryError {
    /// The selected path does not resolve to a regular executable file.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "core_installation.errors.not_executable",
            &[("path", .0.display().to_string())]
        )
    )]
    NotExecutable(PathBuf),
    /// The executable path could not be canonicalized.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "core_installation.errors.canonicalize",
            &[
                ("path", path.display().to_string()),
                ("error", source.to_string())
            ]
        )
    )]
    Canonicalize {
        /// User-selected path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// The version command failed, timed out, or returned no useful output.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "core_installation.errors.probe",
            &[
                ("kind", kind.to_string()),
                ("path", path.display().to_string()),
                ("error", message.clone())
            ]
        )
    )]
    Probe {
        /// Expected core implementation.
        kind: CoreKind,
        /// Executable being checked.
        path: PathBuf,
        /// Concrete command failure.
        message: String,
    },
}

/// Validates that a path is executable, responsive, and belongs to `kind`.
///
/// The executable is invoked only with its version flag and is terminated after
/// five seconds. Successfully running that native command also proves that the
/// operating system can load the binary on the current architecture.
///
/// # Errors
///
/// Returns [`CoreBinaryError`] when the file is missing, not executable, times
/// out, exits unsuccessfully, or reports the wrong core identity.
pub fn validate_core_binary(
    kind: CoreKind,
    path: impl Into<PathBuf>,
) -> Result<CoreBinaryInfo, CoreBinaryError> {
    let path = path.into();
    if !is_executable_file(&path) {
        return Err(CoreBinaryError::NotExecutable(path));
    }
    let canonical = path
        .canonicalize()
        .map_err(|source| CoreBinaryError::Canonicalize {
            path: path.clone(),
            source,
        })?;
    let output = platform_command::output_path_with_timeout(
        &canonical,
        &[OsStr::new("-v")],
        VERSION_TIMEOUT,
    )
    .map_err(|message| CoreBinaryError::Probe {
        kind,
        path: canonical.clone(),
        message,
    })?;
    let combined = version_output(&output.stdout, &output.stderr);
    if !output.status.success() {
        return Err(CoreBinaryError::Probe {
            kind,
            path: canonical,
            message: format!("版本命令退出状态为 {}：{combined}", output.status),
        });
    }
    let version = combined
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .ok_or_else(|| CoreBinaryError::Probe {
            kind,
            path: canonical.clone(),
            message: "版本命令没有返回内容".into(),
        })?
        .to_owned();
    let detected = detect_core_kind(&combined).ok_or_else(|| CoreBinaryError::Probe {
        kind,
        path: canonical.clone(),
        message: format!("无法从版本输出识别内核：{version}"),
    })?;

    Ok(CoreBinaryInfo {
        kind: detected,
        path: canonical,
        version,
        architecture: std::env::consts::ARCH,
    })
}

fn version_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stdout.trim(), stderr.trim()) {
        ("", stderr) => stderr.to_owned(),
        (stdout, "") => stdout.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn detect_core_kind(output: &str) -> Option<CoreKind> {
    let normalized = output.to_ascii_lowercase();
    if normalized.contains("mihomo") || normalized.contains("clash meta") {
        Some(CoreKind::Mihomo)
    } else {
        None
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Verifies a Local startup executable without executing it or changing permissions.
///
/// Unix set-user-ID, set-group-ID and Linux file capabilities are rejected. A
/// root-owned ordinary executable is allowed: ownership alone does not elevate
/// its child. This check is not authorization to adopt a protected service copy.
/// Run on a background worker, before invoking executable validation.
///
/// # Errors
/// Returns an error for non-files, unreadable metadata/capabilities, or privilege
/// attributes. No native authorization, permission change or process is started.
pub fn verify_ordinary_local_executable(
    binary: &std::path::Path,
) -> crate::mihomo::errors::MihomoResult<()> {
    use crate::mihomo::errors::MihomoError;

    let file =
        std::fs::File::open(binary).map_err(|error| MihomoError::Process(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    if !metadata.is_file() {
        return Err(MihomoError::InvalidInput(zenclash_i18n::text(
            "startup.local_privileged",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o6000 != 0 {
            return Err(MihomoError::InvalidInput(zenclash_i18n::text(
                "startup.local_privileged",
            )));
        }
    }
    #[cfg(target_os = "linux")]
    verify_no_linux_capabilities(&file)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_no_linux_capabilities(file: &std::fs::File) -> crate::mihomo::errors::MihomoResult<()> {
    use std::os::fd::AsRawFd;
    read_linux_capabilities(file.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn read_linux_capabilities(fd: std::os::fd::RawFd) -> crate::mihomo::errors::MihomoResult<()> {
    use crate::mihomo::errors::MihomoError;
    // SAFETY: the descriptor remains open; this only asks for the attribute size
    // with a valid static C name and no output buffer.
    let size =
        unsafe { libc::fgetxattr(fd, c"security.capability".as_ptr(), std::ptr::null_mut(), 0) };
    if size == 0 {
        return Ok(());
    }
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
            return Ok(());
        }
        return Err(MihomoError::Process(error.to_string()));
    }
    Err(MihomoError::InvalidInput(zenclash_i18n::text(
        "startup.local_privileged",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_core_names_case_insensitively() {
        assert_eq!(
            detect_core_kind("Mihomo Meta v1.19"),
            Some(CoreKind::Mihomo)
        );
        assert_eq!(detect_core_kind("Other Proxy 0.21"), None);
    }

    #[test]
    fn rejects_unrecognized_version_output() {
        assert_eq!(detect_core_kind("another proxy 1.0"), None);
    }

    #[cfg(unix)]
    #[test]
    fn validates_an_executable_with_the_expected_identity() {
        let path = version_script("valid-mihomo", "Mihomo Meta v1.19.30");

        let info = validate_core_binary(CoreKind::Mihomo, &path).unwrap();

        assert_eq!(info.kind, CoreKind::Mihomo);
        assert_eq!(info.version, "Mihomo Meta v1.19.30");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    fn version_script(name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path =
            std::env::temp_dir().join(format!("zenclash-core-probe-{name}-{}", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_capability_read_failure_is_not_absence() {
        assert!(read_linux_capabilities(-1).is_err());
    }
}
