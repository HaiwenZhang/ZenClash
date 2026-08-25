//! Native desktop system-proxy discovery and control.

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod command;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(any(target_os = "windows", test))]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use unsupported as platform;
#[cfg(target_os = "windows")]
use windows as platform;

use crate::{MihomoError, MihomoResult};

/// Current HTTP and HTTPS proxy state reported by the desktop operating system.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemProxyStatus {
    /// Human-readable network service or desktop proxy backend name.
    pub service: String,
    /// Whether the HTTP proxy is enabled.
    pub enabled: bool,
    /// HTTP proxy host.
    pub server: String,
    /// HTTP proxy port.
    pub port: u16,
    /// Whether the HTTPS proxy is enabled.
    pub secure_enabled: bool,
    /// HTTPS proxy host.
    pub secure_server: String,
    /// HTTPS proxy port.
    pub secure_port: u16,
}

/// Controller for the platform's active desktop system-proxy service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxyManager {
    service: String,
}

impl SystemProxyManager {
    /// Detects the desktop proxy backend and its active network service.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform is unsupported or its native proxy
    /// command is unavailable.
    pub fn detect() -> MihomoResult<Self> {
        platform::detect().map(|service| Self { service })
    }

    /// Returns the human-readable service selected during detection.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Reads the current HTTP and HTTPS proxy settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform command fails or its settings cannot
    /// be queried.
    pub fn status(&self) -> MihomoResult<SystemProxyStatus> {
        platform::status(&self.service)
    }

    /// Enables or disables the HTTP and HTTPS proxy for this service.
    ///
    /// When enabling, the backend disables the active proxy before writing all
    /// values and only re-enables it after every write succeeds. This favors a
    /// safe disabled state over exposing a partially configured proxy.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty host, a zero port, or a failed native
    /// platform command.
    pub fn set_enabled(&self, enabled: bool, server: &str, port: u16) -> MihomoResult<()> {
        if enabled && (server.trim().is_empty() || port == 0) {
            return Err(MihomoError::Process("系统代理地址或端口无效".into()));
        }
        platform::set_enabled(&self.service, enabled, server, port)
    }
}

#[cfg(test)]
mod tests {
    use super::SystemProxyManager;

    #[test]
    fn rejects_invalid_proxy_endpoint_before_platform_command() {
        let manager = SystemProxyManager {
            service: "test-service".into(),
        };

        assert!(manager.set_enabled(true, "   ", 7890).is_err());
        assert!(manager.set_enabled(true, "127.0.0.1", 0).is_err());
    }
}
