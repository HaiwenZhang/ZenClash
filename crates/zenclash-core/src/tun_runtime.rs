//! Platform readback for Mihomo TUN device and route state.

use crate::{CapabilityState, TunConfig};

/// Independent operating-system evidence for an enabled TUN configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunRuntimeObservation {
    /// Configured device name used for native readback.
    pub device_name: Option<String>,
    /// Whether the configured virtual interface exists and is up.
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

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
        if !device_name.starts_with("utun") {
            return observation(
                device_name,
                CapabilityState::Inactive,
                CapabilityState::Unknown,
                None,
            );
        }
        let device = command_success("/sbin/ifconfig", &[device_name]);
        let (route, interface) = route_interface("/sbin/route", &["-n", "get", "1.1.1.1"]);
        observation(
            device_name,
            device,
            route_state(interface.as_deref(), device_name, route),
            interface.as_deref(),
        )
    }

    fn route_interface(command: &str, args: &[&str]) -> (CapabilityState, Option<String>) {
        match crate::platform_command::output(command, args) {
            Ok(output) if output.status.success() => (
                CapabilityState::Active,
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|output| parse_value_after(&output, "interface:").map(str::to_owned)),
            ),
            Ok(_) => (CapabilityState::Inactive, None),
            Err(_) => (CapabilityState::Unknown, None),
        }
    }

    fn command_success(command: &str, args: &[&str]) -> CapabilityState {
        match crate::platform_command::output(command, args) {
            Ok(output) if output.status.success() => CapabilityState::Active,
            Ok(_) => CapabilityState::Inactive,
            Err(_) => CapabilityState::Unknown,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
        let device = command_success("ip", &["link", "show", "dev", device_name]);
        let interface =
            match crate::platform_command::output("ip", &["-4", "route", "get", "1.1.1.1"]) {
                Ok(output) if output.status.success() => String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|output| token_after(&output, "dev").map(str::to_owned)),
                _ => None,
            };
        let route_readback = if interface.is_some() {
            CapabilityState::Active
        } else {
            CapabilityState::Unknown
        };
        observation(
            device_name,
            device,
            route_state(interface.as_deref(), device_name, route_readback),
            interface.as_deref(),
        )
    }

    fn command_success(command: &str, args: &[&str]) -> CapabilityState {
        match crate::platform_command::output(command, args) {
            Ok(output) if output.status.success() => CapabilityState::Active,
            Ok(_) => CapabilityState::Inactive,
            Err(_) => CapabilityState::Unknown,
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use serde::Deserialize;

    use super::*;

    const SCRIPT: &str = concat!(
        "$ErrorActionPreference='Stop'; ",
        "$name=$args[0]; ",
        "$adapter=Get-NetAdapter -Name $name -IncludeHidden -ErrorAction SilentlyContinue | Select-Object -First 1; ",
        "$route=Find-NetRoute -RemoteIPAddress '1.1.1.1' | Where-Object { $_.PSObject.Properties.Name -contains 'DestinationPrefix' } | Select-Object -First 1; ",
        "[pscustomobject]@{deviceStatus=if($null -eq $adapter){$null}else{$adapter.Status.ToString()};routeAlias=if($null -eq $route){$null}else{$route.InterfaceAlias}} | ConvertTo-Json -Compress"
    );

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WindowsObservation {
        device_status: Option<String>,
        route_alias: Option<String>,
    }

    pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
        let result = crate::platform_command::output(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                SCRIPT,
                device_name,
            ],
        );
        let Ok(output) = result else {
            return observation(
                device_name,
                CapabilityState::Unknown,
                CapabilityState::Unknown,
                None,
            );
        };
        if !output.status.success() {
            return observation(
                device_name,
                CapabilityState::Unknown,
                CapabilityState::Unknown,
                None,
            );
        }
        let Ok(readback) = serde_json::from_slice::<WindowsObservation>(&output.stdout) else {
            return observation(
                device_name,
                CapabilityState::Unknown,
                CapabilityState::Unknown,
                None,
            );
        };
        let device = match readback.device_status.as_deref() {
            Some(status) if status.eq_ignore_ascii_case("up") => CapabilityState::Active,
            Some(_) | None => CapabilityState::Inactive,
        };
        let route_readback = if readback.route_alias.is_some() {
            CapabilityState::Active
        } else {
            CapabilityState::Unknown
        };
        observation(
            device_name,
            device,
            route_state(readback.route_alias.as_deref(), device_name, route_readback),
            readback.route_alias.as_deref(),
        )
    }
}

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
