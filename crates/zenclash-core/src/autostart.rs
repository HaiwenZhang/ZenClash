//! Cross-platform login-startup registration with state verification.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use unsupported as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// Current operating-system login-startup registration state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutostartStatus {
    /// Whether any `ZenClash` login-startup entry is enabled.
    pub enabled: bool,
    /// Whether the entry points at the currently running executable.
    pub matches_current_executable: bool,
    /// Platform-specific file, registry, or task location.
    pub location: String,
}

/// Errors produced while reading or changing login-startup registration.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AutostartError {
    /// Filesystem access failed.
    #[error("自动启动 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// The current executable or user configuration directory is unavailable.
    #[error("无法确定自动启动路径：{0}")]
    Path(String),
    /// A required native platform command failed.
    #[error("自动启动系统命令失败：{0}")]
    Command(String),
    /// The operating system did not report the requested state after a write.
    #[error("自动启动写后验证失败：{0}")]
    Verification(String),
}

/// Result type for login-startup registration operations.
pub type AutostartResult<T> = Result<T, AutostartError>;

/// Manages the current executable's native login-startup entry.
#[derive(Clone, Debug)]
pub struct AutostartManager {
    executable: PathBuf,
    entry_path: Option<PathBuf>,
}

impl AutostartManager {
    /// Resolves the running executable and platform-default startup location.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable or user configuration directory
    /// cannot be determined.
    pub fn discover() -> AutostartResult<Self> {
        let executable = std::env::current_exe()
            .map_err(|error| AutostartError::Path(format!("无法读取当前程序路径：{error}")))?;
        let entry_path = platform::default_entry_path()?;
        Ok(Self {
            executable,
            entry_path,
        })
    }

    /// Reads the native login-startup entry without relying on cached settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry or platform service cannot be read.
    pub fn status(&self) -> AutostartResult<AutostartStatus> {
        platform::status(&self.executable, self.entry_path.as_deref())
    }

    /// Enables or disables login startup, then verifies the resulting state.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform update fails or state readback does
    /// not match the request.
    pub fn set_enabled(&self, enabled: bool) -> AutostartResult<AutostartStatus> {
        platform::set_enabled(&self.executable, self.entry_path.as_deref(), enabled)?;
        let status = self.status()?;
        let verified = status.enabled == enabled && (!enabled || status.matches_current_executable);
        if verified {
            Ok(status)
        } else {
            Err(AutostartError::Verification(format!(
                "请求 enabled={enabled}，系统回读 enabled={}、当前程序匹配={}",
                status.enabled, status.matches_current_executable
            )))
        }
    }

    #[cfg(all(test, target_os = "macos"))]
    fn with_entry_path(executable: impl Into<PathBuf>, entry_path: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            entry_path: Some(entry_path.into()),
        }
    }
}

fn home_dir() -> AutostartResult<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| AutostartError::Path("无法确定用户主目录".into()))
}

fn required_entry_path(entry_path: Option<&Path>) -> AutostartResult<&Path> {
    entry_path.ok_or_else(|| AutostartError::Path("当前平台缺少自动启动文件路径".into()))
}
