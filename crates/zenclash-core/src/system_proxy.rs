//! Native desktop system-proxy discovery and control.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod command;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
mod pac;
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

pub use pac::{default_pac_script, normalize_pac_script, PacServer, PacServerStatus};

const MAX_BYPASS_ENTRIES: usize = 64;
const MAX_BYPASS_ENTRY_BYTES: usize = 253;

/// Native system-proxy mode selected by the user.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SystemProxyMode {
    /// Configure explicit HTTP and HTTPS proxy endpoints.
    #[default]
    Manual,
    /// Configure an automatic proxy configuration URL.
    Pac,
}

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
    /// Native proxy-bypass entries reported by the operating system.
    pub bypass: Vec<String>,
    /// Whether automatic proxy configuration is enabled.
    pub auto_enabled: bool,
    /// Automatic proxy configuration URL reported by the OS.
    pub auto_url: String,
}

impl SystemProxyStatus {
    /// Returns whether either manual or automatic system proxying is active.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.auto_enabled || self.enabled || self.secure_enabled
    }
}

/// Controller for the platform's active desktop system-proxy service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxyManager {
    service: String,
}

/// Coordinates the native proxy backend with the process-local PAC service.
#[derive(Clone, Debug, Default)]
pub struct SystemProxyController {
    pac_server: PacServer,
}

impl SystemProxyController {
    /// Creates a controller backed by a shared PAC service owner.
    #[must_use]
    pub fn new(pac_server: PacServer) -> Self {
        Self { pac_server }
    }

    /// Applies the selected proxy mode and verifies native state after writing.
    ///
    /// The PAC listener is started before its URL becomes visible to the OS and
    /// remains alive if disabling the native proxy fails. Switching to manual
    /// mode stops the obsolete PAC listener only after the OS accepts the new
    /// endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error from validation, PAC startup, platform detection, the
    /// native write, or state readback.
    pub fn set_enabled(
        &self,
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> MihomoResult<()> {
        let manager = SystemProxyManager::detect()?;
        if !enabled {
            manager.set_enabled_with_bypass(false, "", 0, &[])?;
            self.pac_server.stop();
            return Ok(());
        }
        match mode {
            SystemProxyMode::Manual => {
                manager.set_enabled_with_bypass(true, host, port, bypass)?;
                self.pac_server.stop();
            }
            SystemProxyMode::Pac => {
                let pac = self.pac_server.start(host, pac_script, port)?;
                if let Err(error) = manager.set_pac_enabled(true, &pac.url) {
                    self.pac_server.stop();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Returns the process-local PAC listener status, when present.
    #[must_use]
    pub fn pac_status(&self) -> Option<PacServerStatus> {
        self.pac_server.status()
    }
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
        self.set_enabled_with_bypass(enabled, server, port, &default_system_proxy_bypass())
    }

    /// Enables or disables the HTTP and HTTPS proxy with explicit bypass rules.
    ///
    /// Entries are normalized, validated, deduplicated, written through the
    /// native backend, and then read back. A successful return therefore means
    /// the requested endpoint and bypass list are observable from the OS.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid endpoint or bypass rule, a failed native
    /// command, or a state that does not match after the write.
    pub fn set_enabled_with_bypass(
        &self,
        enabled: bool,
        server: &str,
        port: u16,
        bypass: &[String],
    ) -> MihomoResult<()> {
        let server = if enabled {
            if port == 0 {
                return Err(MihomoError::Process("系统代理端口不能为 0".into()));
            }
            normalize_system_proxy_host(server)?
        } else {
            String::new()
        };
        let bypass = normalize_system_proxy_bypass(bypass)?;
        platform::set_enabled(&self.service, enabled, &server, port, &bypass)?;
        let status = self.status()?;
        if !enabled {
            if status.active() {
                return Err(MihomoError::Process(
                    "系统代理关闭后状态回读仍显示为启用".into(),
                ));
            }
            return Ok(());
        }
        let endpoint_matches = status.enabled
            && status.secure_enabled
            && status.server == server
            && status.secure_server == server
            && status.port == port
            && status.secure_port == port;
        if !endpoint_matches || status.bypass != bypass {
            return Err(MihomoError::Process(format!(
                "系统代理写后回读不一致：期望 {server}:{port} / {:?}，实际 HTTP {}:{}、HTTPS {}:{} / {:?}",
                bypass,
                status.server,
                status.port,
                status.secure_server,
                status.secure_port,
                status.bypass
            )));
        }
        Ok(())
    }

    /// Enables an operating-system automatic proxy configuration URL.
    ///
    /// Enabling PAC first disables manual HTTP/HTTPS proxying. The resulting
    /// automatic URL is read back before this method reports success.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid HTTP URL, a failed native command, or a
    /// state that does not match after the write.
    pub fn set_pac_enabled(&self, enabled: bool, url: &str) -> MihomoResult<()> {
        let url = if enabled {
            normalize_pac_url(url)?
        } else {
            String::new()
        };
        platform::set_pac_enabled(&self.service, enabled, &url)?;
        let status = self.status()?;
        if enabled {
            if !status.auto_enabled || status.auto_url != url {
                return Err(MihomoError::Process(format!(
                    "PAC 写后回读不一致：期望 {url}，实际启用={}、URL={}",
                    status.auto_enabled, status.auto_url
                )));
            }
        } else if status.active() {
            return Err(MihomoError::Process(
                "系统代理关闭后状态回读仍显示为启用".into(),
            ));
        }
        Ok(())
    }
}

/// Trims and validates a host used by manual proxying or the PAC listener.
///
/// # Errors
///
/// Returns an error for an empty, oversized, or syntactically unsafe host.
pub fn normalize_system_proxy_host(host: &str) -> MihomoResult<String> {
    let host = host.trim();
    if host.is_empty() || host.len() > MAX_BYPASS_ENTRY_BYTES {
        return Err(MihomoError::Process("系统代理主机无效".into()));
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if address.is_unspecified() {
            return Err(MihomoError::Process("系统代理主机不能是未指定地址".into()));
        }
        return Ok(host.to_owned());
    }
    let valid = !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        });
    if !valid {
        return Err(MihomoError::Process(format!(
            "系统代理主机格式无效：{host}"
        )));
    }
    Ok(host.to_owned())
}

fn normalize_pac_url(value: &str) -> MihomoResult<String> {
    let url = reqwest::Url::parse(value.trim())
        .map_err(|error| MihomoError::Process(format!("PAC URL 无效：{error}")))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(MihomoError::Process(
            "PAC URL 必须是无凭据的 HTTP 地址".into(),
        ));
    }
    Ok(url.to_string())
}

/// Returns the platform-appropriate default system-proxy bypass list.
#[must_use]
pub fn default_system_proxy_bypass() -> Vec<String> {
    #[cfg(target_os = "windows")]
    const DEFAULTS: &[&str] = &[
        "localhost",
        "127.*",
        "10.*",
        "172.16.*",
        "172.17.*",
        "172.18.*",
        "172.19.*",
        "172.20.*",
        "172.21.*",
        "172.22.*",
        "172.23.*",
        "172.24.*",
        "172.25.*",
        "172.26.*",
        "172.27.*",
        "172.28.*",
        "172.29.*",
        "172.30.*",
        "172.31.*",
        "192.168.*",
        "<local>",
    ];
    #[cfg(target_os = "macos")]
    const DEFAULTS: &[&str] = &[
        "127.0.0.1",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "localhost",
        "*.local",
        "*.crashlytics.com",
        "<local>",
    ];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const DEFAULTS: &[&str] = &[
        "localhost",
        "127.0.0.1",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "::1",
    ];
    DEFAULTS.iter().map(|entry| (*entry).to_owned()).collect()
}

/// Trims, validates, and case-insensitively deduplicates proxy-bypass entries.
///
/// Empty lines are ignored so this function can consume a multiline editor
/// directly. The accepted syntax covers IP addresses, CIDR ranges, hostnames,
/// wildcard host patterns, and the native `<local>` marker.
///
/// # Errors
///
/// Returns an error when there are too many rules or an entry is oversized,
/// syntactically unsafe, or contains no alphanumeric host/IP content.
pub fn normalize_system_proxy_bypass(entries: &[String]) -> MihomoResult<Vec<String>> {
    let mut normalized = Vec::with_capacity(entries.len().min(MAX_BYPASS_ENTRIES));
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.len() > MAX_BYPASS_ENTRY_BYTES {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则超过 {MAX_BYPASS_ENTRY_BYTES} 字节：{entry}"
            )));
        }
        if !is_valid_bypass_entry(entry) {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则格式无效：{entry}"
            )));
        }
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(entry))
        {
            continue;
        }
        if normalized.len() >= MAX_BYPASS_ENTRIES {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则最多支持 {MAX_BYPASS_ENTRIES} 条"
            )));
        }
        normalized.push(entry.to_owned());
    }
    Ok(normalized)
}

fn is_valid_bypass_entry(entry: &str) -> bool {
    if entry.eq_ignore_ascii_case("<local>") || entry.parse::<IpAddr>().is_ok() {
        return true;
    }
    if let Some((address, prefix)) = entry.split_once('/') {
        if address.contains('/') || prefix.contains('/') {
            return false;
        }
        let Ok(address) = address.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix) = prefix.parse::<u8>() else {
            return false;
        };
        return prefix <= if address.is_ipv4() { 32 } else { 128 };
    }
    if entry.contains(['<', '>', ':', '[', ']']) || entry.starts_with('.') || entry.ends_with('.') {
        return false;
    }
    entry.split('.').all(|label| {
        !label.is_empty()
            && (label == "*"
                || label
                    .chars()
                    .any(|character| character.is_ascii_alphanumeric()))
            && label.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '*' | '?')
            })
    })
}

#[cfg(test)]
mod tests {
    use super::{normalize_system_proxy_bypass, normalize_system_proxy_host, SystemProxyManager};

    #[test]
    fn rejects_invalid_proxy_endpoint_before_platform_command() {
        let manager = SystemProxyManager {
            service: "test-service".into(),
        };

        assert!(manager.set_enabled(true, "   ", 7890).is_err());
        assert!(manager.set_enabled(true, "127.0.0.1", 0).is_err());
    }

    #[test]
    fn bypass_normalization_trims_ignores_empty_and_deduplicates() {
        let normalized = normalize_system_proxy_bypass(&[
            " localhost ".into(),
            String::new(),
            "LOCALHOST".into(),
            "192.168.0.0/16".into(),
            "*.example.com".into(),
        ])
        .unwrap();

        assert_eq!(normalized, ["localhost", "192.168.0.0/16", "*.example.com"]);
    }

    #[test]
    fn bypass_normalization_rejects_command_and_gvariant_delimiters() {
        let error = normalize_system_proxy_bypass(&["localhost';evil".into()]).unwrap_err();

        assert!(error.to_string().contains("格式无效"));
    }

    #[test]
    fn bypass_normalization_rejects_invalid_cidr_prefix() {
        let error = normalize_system_proxy_bypass(&["192.168.0.0/99".into()]).unwrap_err();

        assert!(error.to_string().contains("格式无效"));
    }

    #[test]
    fn bypass_normalization_accepts_a_wildcard_dns_label_but_not_empty_labels() {
        assert_eq!(
            normalize_system_proxy_bypass(&["*.example.com".into()]).unwrap(),
            ["*.example.com"]
        );
        assert!(normalize_system_proxy_bypass(&["example..com".into()]).is_err());
    }

    #[test]
    fn system_proxy_host_accepts_loopback_and_rejects_unspecified_addresses() {
        assert_eq!(
            normalize_system_proxy_host(" localhost ").unwrap(),
            "localhost"
        );
        assert!(normalize_system_proxy_host("0.0.0.0").is_err());
    }
}
