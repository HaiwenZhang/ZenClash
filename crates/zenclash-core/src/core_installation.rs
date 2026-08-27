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
    #[error("内核文件不可执行：{0}")]
    NotExecutable(PathBuf),
    /// The executable path could not be canonicalized.
    #[error("无法读取内核文件 {path}：{source}")]
    Canonicalize {
        /// User-selected path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// The version command failed, timed out, or returned no useful output.
    #[error("无法检测 {kind} 内核 {path}：{message}")]
    Probe {
        /// Expected core implementation.
        kind: CoreKind,
        /// Executable being checked.
        path: PathBuf,
        /// Concrete command failure.
        message: String,
    },
    /// The executable belongs to the other supported core implementation.
    #[error("内核类型不匹配：期望 {expected}，但 {path} 报告为 {actual}")]
    WrongKind {
        /// Core selected in the UI.
        expected: CoreKind,
        /// Core detected from version output.
        actual: CoreKind,
        /// Executable being checked.
        path: PathBuf,
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
    if detected != kind {
        return Err(CoreBinaryError::WrongKind {
            expected: kind,
            actual: detected,
            path: canonical,
        });
    }
    Ok(CoreBinaryInfo {
        kind,
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
    } else if normalized.contains("meow") {
        Some(CoreKind::Meow)
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
        assert_eq!(detect_core_kind("Meow Meta 0.21"), Some(CoreKind::Meow));
    }

    #[test]
    fn rejects_unrecognized_version_output() {
        assert_eq!(detect_core_kind("another proxy 1.0"), None);
    }

    #[cfg(unix)]
    #[test]
    fn validates_an_executable_with_the_expected_identity() {
        let path = version_script("valid-meow", "Meow Meta 0.21.1");

        let info = validate_core_binary(CoreKind::Meow, &path).unwrap();

        assert_eq!(info.kind, CoreKind::Meow);
        assert_eq!(info.version, "Meow Meta 0.21.1");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_executable_with_the_other_core_identity() {
        let path = version_script("wrong-kind", "Mihomo Meta v1.19.0");

        let error = validate_core_binary(CoreKind::Meow, &path).unwrap_err();

        assert!(matches!(
            error,
            CoreBinaryError::WrongKind {
                expected: CoreKind::Meow,
                actual: CoreKind::Mihomo,
                ..
            }
        ));
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
