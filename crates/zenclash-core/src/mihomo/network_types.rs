//! Pure network-probe value types shared across preferences storage and diagnostics workflows.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Built-in public-IP services compatible with Clash Party's network page.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PublicIpProvider {
    /// `api.ip.sb` `GeoIP` response.
    #[default]
    IpSb,
    /// `ipwho.is` `GeoIP` response.
    IpWhoIs,
    /// `api.ipapi.is` network intelligence response.
    IpApiIs,
}

impl PublicIpProvider {
    /// All providers in stable UI order.
    pub const ALL: [Self; 3] = [Self::IpSb, Self::IpWhoIs, Self::IpApiIs];

    /// Short provider label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::IpSb => "IP.SB",
            Self::IpWhoIs => "ipwho.is",
            Self::IpApiIs => "ipapi.is",
        }
    }

    pub(crate) const fn endpoint(self) -> &'static str {
        match self {
            Self::IpSb => "https://api.ip.sb/geoip",
            Self::IpWhoIs => "https://ipwho.is/",
            Self::IpApiIs => "https://api.ipapi.is/",
        }
    }
}

/// Named HTTP(S) endpoint measured by the network diagnostics page.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkLatencyTarget {
    /// User-facing endpoint name.
    pub name: String,
    /// Absolute HTTP(S) URL.
    pub url: String,
}

impl NetworkLatencyTarget {
    /// Validates and normalizes a latency target.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized name, credentials, unsupported
    /// schemes, missing hosts, or malformed URLs.
    pub fn new(name: impl Into<String>, url: impl AsRef<str>) -> NetworkProbeResult<Self> {
        let name = name.into().trim().to_owned();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(NetworkProbeError::InvalidTarget(
                "探测名称必须为 1 到 64 个字符".into(),
            ));
        }
        let url = normalize_http_url(url.as_ref())?;
        Ok(Self {
            name,
            url: url.into(),
        })
    }
}

/// Default public endpoints for network latency diagnostics.
pub const DEFAULT_NETWORK_LATENCY_TARGETS: [(&str, &str); 5] = [
    ("Google", "https://www.google.com/generate_204"),
    ("Cloudflare", "https://www.cloudflare.com/cdn-cgi/trace"),
    ("GitHub", "https://github.com/"),
    ("OpenAI", "https://chatgpt.com/robots.txt"),
    ("Anthropic", "https://www.anthropic.com/robots.txt"),
];

/// Errors produced while constructing or running network diagnostics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NetworkProbeError {
    /// Caller supplied an unsafe or malformed target.
    #[error(
        "{}",
        zenclash_i18n::text_with("network.probe.errors.invalid_target", &[("error", .0.clone())])
    )]
    InvalidTarget(String),
    /// HTTP client construction or transport failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("network.probe.errors.http", &[("error", .0.to_string())])
    )]
    Http(#[from] reqwest::Error),
    /// A response exceeded the defensive body limit.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "network.probe.errors.response_too_large",
            &[("limit", limit_kib.to_string())]
        )
    )]
    ResponseTooLarge {
        /// Maximum accepted response size in kibibytes.
        limit_kib: usize,
    },
    /// A provider returned successful HTTP with an incompatible document.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "network.probe.errors.invalid_response",
            &[("error", .0.clone())]
        )
    )]
    InvalidResponse(String),
}

/// Result type for network diagnostics.
pub type NetworkProbeResult<T> = Result<T, NetworkProbeError>;

pub(crate) fn normalize_http_url(value: &str) -> NetworkProbeResult<reqwest::Url> {
    let trimmed = value.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&candidate)
        .map_err(|error| NetworkProbeError::InvalidTarget(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NetworkProbeError::InvalidTarget(
            "仅支持带主机名的 HTTP 或 HTTPS URL".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetworkProbeError::InvalidTarget(
            "探测 URL 不允许包含登录凭据".into(),
        ));
    }
    Ok(url)
}
