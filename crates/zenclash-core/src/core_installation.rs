//! Runtime-core executable discovery and identity validation.

use std::{ffi::OsStr, path::PathBuf, time::Duration};

use thiserror::Error;

use crate::{CoreKind, platform_command};

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
}
