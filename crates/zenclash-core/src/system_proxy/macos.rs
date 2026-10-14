#[cfg(target_os = "macos")]
use std::process::Output;

#[cfg(target_os = "macos")]
use super::SystemProxyStatus;
#[cfg(target_os = "macos")]
use crate::{
    MihomoResult, SystemProxyBackendKind, SystemProxyBackendUnavailable, SystemProxyError,
};

#[cfg(target_os = "macos")]
mod snapshot;

#[cfg(target_os = "macos")]
pub(super) use snapshot::{ProxySnapshot, apply, capture, restore};

#[cfg(target_os = "macos")]
pub(super) fn detect() -> MihomoResult<String> {
    if let Ok(service) = std::env::var("ZENCLASH_NETWORK_SERVICE")
        && !service.trim().is_empty()
    {
        return Ok(service);
    }
    if let Some(service) = active_network_service() {
        return Ok(service);
    }
    let output = run_networksetup(["-listallnetworkservices"])?;
    let services = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("An asterisk") && !line.starts_with('*')
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    services
        .iter()
        .find(|service| service.as_str() == "Wi-Fi")
        .or_else(|| services.iter().find(|service| service.contains("Ethernet")))
        .or_else(|| services.first())
        .cloned()
        .ok_or_else(|| {
            SystemProxyError::BackendUnavailable {
                backend: SystemProxyBackendKind::MacOs,
                reason: SystemProxyBackendUnavailable::NoUsableService,
            }
            .into()
        })
}

#[cfg(target_os = "macos")]
pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    capture(super::native::NativeProxyTarget::Service(service))
        .map(|snapshot| snapshot.summary().clone())
}

#[cfg(target_os = "macos")]
fn active_network_service() -> Option<String> {
    let route = crate::platform_command::output("/sbin/route", &["-n", "get", "default"])
        .ok()
        .filter(|output| output.status.success())?;
    let interface = parse_default_interface(&route.stdout)?;
    let order = run_networksetup(["-listnetworkserviceorder"]).ok()?;
    parse_service_for_interface(&order.stdout, &interface)
}

#[cfg(target_os = "macos")]
pub(super) fn set_enabled(
    service: &str,
    enabled: bool,
    server: &str,
    port: u16,
    bypass: &[String],
) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Manual {
            host: server,
            port,
            bypass,
        }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}

#[cfg(target_os = "macos")]
pub(super) fn set_pac_enabled(service: &str, enabled: bool, url: &str) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Pac { url }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}

#[cfg(target_os = "macos")]
fn run_networksetup<const N: usize>(args: [&str; N]) -> MihomoResult<Output> {
    run_networksetup_slice(&args)
}

#[cfg(target_os = "macos")]
fn run_networksetup_slice(args: &[&str]) -> MihomoResult<Output> {
    let output = crate::platform_command::output_typed("/usr/sbin/networksetup", args)
        .map_err(SystemProxyError::DiscoveryCommand)?;
    if output.status.success() {
        return Ok(output);
    }

    Err(SystemProxyError::DiscoveryExit {
        status: output.status,
    }
    .into())
}

fn parse_default_interface(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        line.trim()
            .strip_prefix("interface:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_service_for_interface(output: &[u8], interface: &str) -> Option<String> {
    let output = String::from_utf8_lossy(output);
    let mut service = None;
    for line in output.lines().map(str::trim) {
        if let Some((order, name)) = line
            .strip_prefix('(')
            .and_then(|line| line.split_once(") "))
            && order.chars().all(|character| character.is_ascii_digit())
        {
            service = Some(name.trim_start_matches('*').trim().to_owned());
            continue;
        }
        let device = line
            .split_once("Device:")
            .map(|(_, value)| value.trim().trim_end_matches(')').trim());
        if device == Some(interface) {
            return service.filter(|service| !service.is_empty());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_default_interface, parse_service_for_interface};
    #[test]
    fn resolves_default_route_interface_to_network_service() {
        let route = b"route to: default\ninterface: en7\n";
        let order = b"(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n(2) USB Ethernet\n(Hardware Port: USB Ethernet, Device: en7)\n";
        let interface = parse_default_interface(route).unwrap();
        assert_eq!(
            parse_service_for_interface(order, &interface).as_deref(),
            Some("USB Ethernet")
        );
    }
}
