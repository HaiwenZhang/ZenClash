//! Protocol-specific display facts; these do not authorize native restoration.
/// Display summary of configured HTTP, HTTPS and PAC settings.
///
/// Native attributes outside this projection are retained by private snapshots;
/// this record is not a complete recovery or ownership proof.
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
    /// Returns whether HTTP, HTTPS or PAC is enabled in this display projection.
    ///
    /// Other platform protocols and global flags belong to the native snapshot.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.auto_enabled || self.enabled || self.secure_enabled
    }
}

/// Display projection of HTTP, HTTPS and PAC settings.
///
/// The legacy record name and fields remain compatible. Complete native input
/// snapshots, global platform flags and ownership evidence are separate.
pub type SystemProxySummary = SystemProxyStatus;

/// One protocol's configured endpoint and its independent enabled flag.
///
/// A disabled endpoint can retain a cached host and port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyEndpointStatus<'a> {
    enabled: bool,
    host: &'a str,
    port: u16,
}

impl<'a> ProxyEndpointStatus<'a> {
    /// Returns whether the observed protocol is enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns the configured host, including a disabled cached value.
    #[must_use]
    pub const fn host(self) -> &'a str {
        self.host
    }

    /// Returns the configured port, including a disabled cached value.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// Observed PAC enablement and its independently retained configuration URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacProxyStatus<'a> {
    enabled: bool,
    url: &'a str,
}

impl<'a> PacProxyStatus<'a> {
    /// Returns whether the observed PAC setting is enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns the configured URL, including a disabled cached value.
    #[must_use]
    pub const fn url(self) -> &'a str {
        self.url
    }
}

impl SystemProxyStatus {
    /// Reads the HTTP projection without copying or normalizing native values.
    #[must_use]
    pub fn http(&self) -> ProxyEndpointStatus<'_> {
        ProxyEndpointStatus {
            enabled: self.enabled,
            host: &self.server,
            port: self.port,
        }
    }

    /// Reads the HTTPS projection without confusing it with HTTP enablement.
    #[must_use]
    pub fn https(&self) -> ProxyEndpointStatus<'_> {
        ProxyEndpointStatus {
            enabled: self.secure_enabled,
            host: &self.secure_server,
            port: self.secure_port,
        }
    }

    /// Reads PAC enablement and cached URL separately.
    #[must_use]
    pub fn pac(&self) -> PacProxyStatus<'_> {
        PacProxyStatus {
            enabled: self.auto_enabled,
            url: &self.auto_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_views_preserve_asymmetric_flags_and_inactive_cached_values() {
        let summary = SystemProxySummary {
            enabled: false,
            server: "cached-http".into(),
            port: 8080,
            secure_enabled: true,
            secure_server: "active-https".into(),
            secure_port: 8443,
            auto_enabled: false,
            auto_url: "http://cached.test/proxy.pac".into(),
            ..Default::default()
        };
        assert!(!summary.http().is_enabled());
        assert_eq!(
            (summary.http().host(), summary.http().port()),
            ("cached-http", 8080)
        );
        assert!(summary.https().is_enabled());
        assert_eq!(
            (summary.https().host(), summary.https().port()),
            ("active-https", 8443)
        );
        assert!(!summary.pac().is_enabled());
        assert_eq!(summary.pac().url(), "http://cached.test/proxy.pac");
        assert!(summary.active());
    }
}
