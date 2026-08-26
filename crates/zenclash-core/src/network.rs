//! Cross-platform discovery of the host's active network path.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod command;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
mod probe;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(any(target_os = "windows", test))]
mod windows;

pub use probe::{
    NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeError, NetworkProbeResult,
    NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot, PublicIpInfo, PublicIpProvider,
    DEFAULT_NETWORK_LATENCY_TARGETS,
};

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use unsupported as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// Snapshot of the operating system's active default network path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemNetworkSnapshot {
    /// Active interface or adapter name.
    pub interface: String,
    /// IPv4 default gateway, when the route exposes one.
    pub gateway: String,
    /// IPv4 address assigned to the active interface.
    pub local_ipv4: String,
    /// DNS servers reported by the operating system.
    pub dns_servers: Vec<String>,
    /// Hard failure or partial-discovery warning.
    pub error: Option<String>,
}

impl SystemNetworkSnapshot {
    /// Detects the active interface, gateway, address and DNS servers.
    ///
    /// Hard failures produce an otherwise empty snapshot. Optional command
    /// failures retain successfully discovered fields and populate
    /// [`Self::error`] with a warning.
    #[must_use]
    pub fn detect() -> Self {
        platform::detect().unwrap_or_else(Self::failed)
    }

    fn failed(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

fn warning_message(warnings: &[String]) -> Option<String> {
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("；"))
    }
}

fn unique_nonempty(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if !value.is_empty() && !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::unique_nonempty;

    #[test]
    fn unique_nonempty_preserves_first_seen_order() {
        assert_eq!(
            unique_nonempty([
                " 8.8.8.8 ".to_owned(),
                String::new(),
                "1.1.1.1".to_owned(),
                "8.8.8.8".to_owned(),
            ]),
            ["8.8.8.8".to_owned(), "1.1.1.1".to_owned()]
        );
    }
}
