//! Platform readback for Mihomo TUN device and route state.

use crate::{CapabilityState, TunConfig};

#[cfg(all(test, any(unix, windows)))]
mod test_support;

/// Independent operating-system evidence for an enabled TUN configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunRuntimeObservation {
    /// Configured device name used for native readback.
    pub device_name: Option<String>,
    /// Whether the configured virtual interface exists and is administratively up.
    /// This evidence alone does not prove packet forwarding or carrier availability.
    pub device: CapabilityState,
    /// Whether a representative public IPv4 route resolves to that interface.
    pub route: CapabilityState,
    /// Human-readable evidence suitable for the interactive diagnostics UI.
    pub detail: String,
}

impl TunRuntimeObservation {
    fn inactive() -> Self {
        Self {
            device_name: None,
            device: CapabilityState::Inactive,
            route: CapabilityState::Inactive,
            detail: "TUN 未配置启用".into(),
        }
    }

    fn unnamed() -> Self {
        Self {
            device_name: None,
            device: CapabilityState::Unknown,
            route: CapabilityState::Unknown,
            detail: "Mihomo 未回读 TUN 设备名，无法把系统设备和路由归属到当前内核".into(),
        }
    }
}

/// Reads native TUN device and route facts without changing system state.
pub struct TunRuntimeObserver;

impl TunRuntimeObserver {
    /// Observes the device named by Mihomo and the route selected for `1.1.1.1`.
    ///
    /// A blank runtime device name deliberately remains unknown: enumerating
    /// arbitrary TUN adapters could attribute another VPN's device to Mihomo.
    #[must_use]
    pub fn observe(config: &TunConfig) -> TunRuntimeObservation {
        if !config.enable {
            return TunRuntimeObservation::inactive();
        }
        let device = config.device.trim();
        if device.is_empty() {
            return TunRuntimeObservation::unnamed();
        }
        platform::observe(device)
    }
}

fn state_label(state: CapabilityState) -> &'static str {
    match state {
        CapabilityState::Inactive => "inactive",
        CapabilityState::Active => "active",
        CapabilityState::Unknown => "unknown",
        CapabilityState::Unsupported => "unsupported",
    }
}

fn observation(
    device_name: &str,
    device: CapabilityState,
    route: CapabilityState,
    route_interface: Option<&str>,
) -> TunRuntimeObservation {
    TunRuntimeObservation {
        device_name: Some(device_name.to_owned()),
        device,
        route,
        detail: format!(
            "device {device_name} {} · route 1.1.1.1 {}{}",
            state_label(device),
            state_label(route),
            route_interface.map_or_else(String::new, |interface| format!(" via {interface}"))
        ),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_value_after<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(marker)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[cfg(any(target_os = "linux", test))]
fn token_after<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    let mut fields = output.split_whitespace();
    while let Some(field) = fields.next() {
        if field == marker {
            return fields.next();
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    use super::*;

    pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
        observation(
            device_name,
            CapabilityState::Unsupported,
            CapabilityState::Unsupported,
            None,
        )
    }
}

fn route_state(
    observed_interface: Option<&str>,
    expected_interface: &str,
    readback: CapabilityState,
) -> CapabilityState {
    match observed_interface {
        Some(interface) if interface.eq_ignore_ascii_case(expected_interface) => {
            CapabilityState::Active
        }
        Some(_) => CapabilityState::Inactive,
        None => match readback {
            CapabilityState::Active => CapabilityState::Unknown,
            state => state,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_device_never_claims_another_vpn_adapter() {
        let observation = TunRuntimeObserver::observe(&TunConfig {
            enable: true,
            device: "  ".into(),
            auto_route: true,
            ..TunConfig::default()
        });

        assert_eq!(observation.device, CapabilityState::Unknown);
        assert_eq!(observation.route, CapabilityState::Unknown);
        assert!(observation.device_name.is_none());
    }

    #[test]
    fn platform_route_parsers_preserve_exact_interface_identity() {
        assert_eq!(
            parse_value_after("gateway: 192.0.2.1\ninterface: utun42\n", "interface:"),
            Some("utun42")
        );
        assert_eq!(
            token_after("1.1.1.1 dev Mihomo table 2022 src 198.18.0.1", "dev"),
            Some("Mihomo")
        );
        assert_eq!(
            route_state(Some("utun42"), "utun42", CapabilityState::Active),
            CapabilityState::Active
        );
        assert_eq!(
            route_state(Some("en0"), "utun42", CapabilityState::Active),
            CapabilityState::Inactive
        );
    }
}
