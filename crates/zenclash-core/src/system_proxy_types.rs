//! Shared system-proxy value types and pure input validators.
//!
//! Kept free of preference-store and OS-backend dependencies so `preferences`
//! and `system_proxy` both depend downward on this leaf module without cycles.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::core_errors::{
    MihomoResult, SystemProxySetting, SystemProxyValidationError, invalid_setting,
};

pub(crate) const MAX_BYPASS_ENTRIES: usize = 64;
pub(crate) const MAX_BYPASS_ENTRY_BYTES: usize = 253;
pub(crate) const MAX_PAC_SCRIPT_BYTES: usize = 256 * 1024;
const DEFAULT_PAC_SCRIPT: &str = r#"function FindProxyForURL(url, host) {
  return "PROXY 127.0.0.1:%mixed-port%; SOCKS5 127.0.0.1:%mixed-port%; DIRECT;";
}
"#;

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

/// Persisted managed-endpoint claim, separate from complete in-memory native snapshots.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SystemProxyOwnership {
    /// Verified manual HTTP/HTTPS endpoint and bypass rules.
    Manual {
        /// Native network service selected during the write.
        service: String,
        /// Normalized HTTP/HTTPS proxy host.
        host: String,
        /// HTTP/HTTPS proxy port.
        port: u16,
        /// Normalized bypass rules observed after the write.
        bypass: Vec<String>,
    },
    /// Verified automatic proxy URL.
    Pac {
        /// Native network service selected during the write.
        service: String,
        /// PAC URL observed after the write.
        url: String,
    },
}

/// Returns the default PAC document used by a fresh installation.
#[must_use]
pub const fn default_pac_script() -> &'static str {
    DEFAULT_PAC_SCRIPT
}

/// Validates and normalizes a user-supplied PAC document.
///
/// # Errors
///
/// Returns an error for an empty/oversized document, embedded NUL bytes, or a
/// document that does not define `FindProxyForURL`.
pub fn normalize_pac_script(script: &str) -> MihomoResult<String> {
    let script = script.trim();
    if script.is_empty() {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::Empty,
        ));
    }
    if script.len() > MAX_PAC_SCRIPT_BYTES {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::TooLong {
                maximum: MAX_PAC_SCRIPT_BYTES,
            },
        ));
    }
    if script.contains('\0') {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::EmbeddedNul,
        ));
    }
    if !script.contains("FindProxyForURL") {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::MissingPacFunction,
        ));
    }
    Ok(format!("{script}\n"))
}

/// Trims and validates a host used by manual proxying or the PAC listener.
///
/// # Errors
///
/// Returns an error for an empty, oversized, or syntactically unsafe host.
pub fn normalize_system_proxy_host(host: &str) -> MihomoResult<String> {
    let host = host.trim();
    if host.is_empty() {
        return Err(invalid_setting(
            SystemProxySetting::Host,
            SystemProxyValidationError::Empty,
        ));
    }
    if host.len() > MAX_BYPASS_ENTRY_BYTES {
        return Err(invalid_setting(
            SystemProxySetting::Host,
            SystemProxyValidationError::TooLong {
                maximum: MAX_BYPASS_ENTRY_BYTES,
            },
        ));
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if address.is_unspecified() {
            return Err(invalid_setting(
                SystemProxySetting::Host,
                SystemProxyValidationError::UnspecifiedAddress,
            ));
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
        return Err(invalid_setting(
            SystemProxySetting::Host,
            SystemProxyValidationError::InvalidSyntax,
        ));
    }
    Ok(host.to_owned())
}

pub(crate) fn normalize_pac_url(value: &str) -> MihomoResult<String> {
    let url = reqwest::Url::parse(value.trim()).map_err(|error| {
        invalid_setting(
            SystemProxySetting::PacUrl,
            SystemProxyValidationError::InvalidPacUrl(error),
        )
    })?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(invalid_setting(
            SystemProxySetting::PacUrl,
            SystemProxyValidationError::UnsafePacUrl,
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
            return Err(invalid_setting(
                SystemProxySetting::Bypass,
                SystemProxyValidationError::TooLong {
                    maximum: MAX_BYPASS_ENTRY_BYTES,
                },
            ));
        }
        if !is_valid_bypass_entry(entry) {
            return Err(invalid_setting(
                SystemProxySetting::Bypass,
                SystemProxyValidationError::InvalidSyntax,
            ));
        }
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(entry))
        {
            continue;
        }
        if normalized.len() >= MAX_BYPASS_ENTRIES {
            return Err(invalid_setting(
                SystemProxySetting::Bypass,
                SystemProxyValidationError::TooMany {
                    maximum: MAX_BYPASS_ENTRIES,
                },
            ));
        }
        normalized.push(entry.to_owned());
    }
    Ok(normalized)
}

pub(crate) fn validate_system_proxy_ownership(
    ownership: &SystemProxyOwnership,
) -> MihomoResult<()> {
    let service = match ownership {
        SystemProxyOwnership::Manual {
            service,
            host,
            port,
            bypass,
        } => {
            if *port == 0 {
                return Err(invalid_setting(
                    SystemProxySetting::Port,
                    SystemProxyValidationError::ZeroPort,
                ));
            }
            if normalize_system_proxy_host(host)? != *host {
                return Err(invalid_setting(
                    SystemProxySetting::Ownership,
                    SystemProxyValidationError::NotCanonical,
                ));
            }
            if normalize_system_proxy_bypass(bypass)? != *bypass {
                return Err(invalid_setting(
                    SystemProxySetting::Ownership,
                    SystemProxyValidationError::NotCanonical,
                ));
            }
            service
        }
        SystemProxyOwnership::Pac { service, url } => {
            if normalize_pac_url(url)? != *url {
                return Err(invalid_setting(
                    SystemProxySetting::Ownership,
                    SystemProxyValidationError::NotCanonical,
                ));
            }
            service
        }
    };
    if service.trim().is_empty() || service.len() > 256 || service.trim() != service {
        return Err(invalid_setting(
            SystemProxySetting::Ownership,
            SystemProxyValidationError::InvalidService,
        ));
    }
    Ok(())
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
