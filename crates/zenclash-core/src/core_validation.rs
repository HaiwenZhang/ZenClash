//! Target-core validation for generated runtime configurations.

use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use thiserror::Error;

use crate::{platform_command, profiles::validate_clash_yaml, CoreKind};

const CONFIG_VALIDATION_TIMEOUT: Duration = Duration::from_secs(30);
static VALIDATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure returned when a concrete runtime core rejects a generated config.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreConfigValidationError {
    /// The configuration path is missing or is not a regular file.
    #[error("待验证配置不是普通文件：{0}")]
    InvalidConfig(PathBuf),
    /// The validation data directory could not be prepared.
    #[error("无法准备内核验证目录 {path}：{source}")]
    PrepareHome {
        /// Writable core home used by `-t`.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// A temporary payload could not be written or removed safely.
    #[error("无法准备临时内核配置 {path}：{message}")]
    TemporaryConfig {
        /// Temporary file involved in the failed operation.
        path: PathBuf,
        /// Concrete filesystem failure.
        message: String,
    },
    /// The native validation command failed to start, timed out, or produced
    /// more output than the bounded command runner accepts.
    #[error("无法使用 {kind} 验证配置 {path}：{message}")]
    Command {
        /// Runtime core used for validation.
        kind: CoreKind,
        /// Configuration passed to the core.
        path: PathBuf,
        /// Concrete command-runner failure.
        message: String,
    },
    /// The target core completed validation but rejected the configuration.
    #[error("{kind} 拒绝配置 {path}（退出状态 {status}）：{message}")]
    Rejected {
        /// Runtime core used for validation.
        kind: CoreKind,
        /// Configuration passed to the core.
        path: PathBuf,
        /// Native process exit status.
        status: String,
        /// Bounded diagnostic emitted by the core.
        message: String,
    },
    /// The payload is not even a structurally valid Clash YAML mapping.
    #[error("配置 YAML 无效：{0}")]
    InvalidYaml(String),
}

/// Reusable validator bound to one concrete runtime binary and writable home.
#[derive(Clone, Debug)]
pub struct CoreConfigValidator {
    kind: CoreKind,
    binary: PathBuf,
    home_dir: PathBuf,
}

impl CoreConfigValidator {
    /// Creates a validator for the same inputs used by a managed core process.
    #[must_use]
    pub fn new(kind: CoreKind, binary: impl Into<PathBuf>, home_dir: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            binary: binary.into(),
            home_dir: home_dir.into(),
        }
    }

    /// Runs the selected core with `-t -d <home> -f <config>`.
    ///
    /// A successful exit that still emits a fatal Mihomo diagnostic is treated
    /// as a rejection. The command is terminated after 30 seconds and its
    /// output is bounded by the shared native command runner.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is missing, the validation process
    /// cannot run, times out, or the selected core rejects the configuration.
    pub fn validate_file(&self, config: impl AsRef<Path>) -> Result<(), CoreConfigValidationError> {
        if !self.kind.capabilities().config_validation {
            return Ok(());
        }
        let config = config.as_ref();
        if !config.is_file() {
            return Err(CoreConfigValidationError::InvalidConfig(
                config.to_path_buf(),
            ));
        }
        std::fs::create_dir_all(&self.home_dir).map_err(|source| {
            CoreConfigValidationError::PrepareHome {
                path: self.home_dir.clone(),
                source,
            }
        })?;
        let output = platform_command::output_path_with_timeout(
            &self.binary,
            &[
                OsStr::new("-t"),
                OsStr::new("-d"),
                self.home_dir.as_os_str(),
                OsStr::new("-f"),
                config.as_os_str(),
            ],
            CONFIG_VALIDATION_TIMEOUT,
        )
        .map_err(|message| CoreConfigValidationError::Command {
            kind: self.kind,
            path: config.to_path_buf(),
            message,
        })?;
        let diagnostic = combined_output(&output.stdout, &output.stderr);
        if output.status.success() && !contains_fatal_diagnostic(&diagnostic) {
            return Ok(());
        }
        Err(CoreConfigValidationError::Rejected {
            kind: self.kind,
            path: config.to_path_buf(),
            status: output.status.to_string(),
            message: non_empty_diagnostic(diagnostic),
        })
    }

    /// Validates an in-memory effective YAML payload with the selected core.
    ///
    /// The payload is written to a private, uniquely named file in the core
    /// home and removed after validation, including failure paths.
    ///
    /// # Errors
    ///
    /// Returns YAML, filesystem, command, timeout, or core-rejection errors.
    pub fn validate_payload(&self, payload: &str) -> Result<(), CoreConfigValidationError> {
        if !self.kind.capabilities().config_validation {
            return Ok(());
        }
        validate_clash_yaml(payload)
            .map_err(|error| CoreConfigValidationError::InvalidYaml(error.to_string()))?;
        std::fs::create_dir_all(&self.home_dir).map_err(|source| {
            CoreConfigValidationError::PrepareHome {
                path: self.home_dir.clone(),
                source,
            }
        })?;
        let path = self.temporary_config_path();
        write_private_file(&path, payload.as_bytes()).map_err(|error| {
            CoreConfigValidationError::TemporaryConfig {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;
        let temporary = TemporaryConfig(path.clone());
        let validation = self.validate_file(&path);
        drop(temporary);
        validation
    }

    fn temporary_config_path(&self) -> PathBuf {
        let sequence = VALIDATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.home_dir.join(format!(
            ".zenclash-check-{}-{sequence}.yaml",
            std::process::id()
        ))
    }
}

struct TemporaryConfig(PathBuf);

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %self.0.display(), "failed to remove temporary core validation config");
            }
        }
    }
}

fn write_private_file(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(payload)?;
    file.sync_all()
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stdout.trim(), stderr.trim()) {
        ("", "") => String::new(),
        ("", stderr) => stderr.to_owned(),
        (stdout, "") => stdout.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn contains_fatal_diagnostic(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("fatal")
        || output.contains("level=fata")
        || output.contains("parse config error")
}

fn non_empty_diagnostic(diagnostic: String) -> String {
    if diagnostic.trim().is_empty() {
        "内核没有返回错误详情".into()
    } else {
        diagnostic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn invokes_the_target_core_with_test_home_and_config_arguments() {
        let root = test_root("accepts");
        let binary = validation_script(
            &root,
            "validator",
            "test \"$1\" = '-t' && test \"$2\" = '-d' && test \"$4\" = '-f'",
        );
        let config = root.join("config.yaml");
        std::fs::write(&config, "rules:\n  - MATCH,DIRECT\n").expect("write config");
        let validator = CoreConfigValidator::new(CoreKind::Mihomo, binary, root.join("home"));

        validator
            .validate_file(config)
            .expect("core accepts config");

        std::fs::remove_dir_all(root).expect("remove test root");
    }

    #[cfg(unix)]
    #[test]
    fn reports_real_core_rejection_diagnostics() {
        let root = test_root("rejects");
        let binary = validation_script(&root, "validator", "echo 'parse config error' >&2; exit 1");
        let config = root.join("config.yaml");
        std::fs::write(&config, "rules:\n  - MATCH,DIRECT\n").expect("write config");
        let validator = CoreConfigValidator::new(CoreKind::Mihomo, binary, root.join("home"));

        let error = validator
            .validate_file(config)
            .expect_err("core rejects config");

        assert!(error.to_string().contains("parse config error"));
        std::fs::remove_dir_all(root).expect("remove test root");
    }

    #[cfg(unix)]
    #[test]
    fn payload_validation_removes_its_private_temporary_file() {
        let root = test_root("payload-cleanup");
        let binary = validation_script(&root, "validator", "exit 0");
        let home = root.join("home");
        let validator = CoreConfigValidator::new(CoreKind::Mihomo, binary, &home);

        validator
            .validate_payload("rules:\n  - MATCH,DIRECT\n")
            .expect("payload accepted");

        let remaining = std::fs::read_dir(&home)
            .expect("read validation home")
            .collect::<Result<Vec<_>, _>>()
            .expect("read entries");
        assert!(
            remaining.is_empty(),
            "temporary files remain: {remaining:?}"
        );
        std::fs::remove_dir_all(root).expect("remove test root");
    }

    #[cfg(unix)]
    fn validation_script(root: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(root).expect("create test root");
        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write validator");
        let mut permissions = std::fs::metadata(&path)
            .expect("validator metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).expect("make validator executable");
        path
    }

    #[cfg(unix)]
    fn test_root(name: &str) -> PathBuf {
        let sequence = VALIDATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "zenclash-core-validation-{name}-{}-{sequence}",
            std::process::id()
        ))
    }
}
