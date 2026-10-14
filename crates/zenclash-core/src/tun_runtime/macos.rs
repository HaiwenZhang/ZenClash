use std::process::Output;

use super::*;

#[cfg(target_os = "macos")]
pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
    observe_with(device_name, crate::platform_command::output)
}

pub(super) fn observe_with(
    device_name: &str,
    mut read: impl FnMut(&str, &[&str]) -> Result<Output, String>,
) -> TunRuntimeObservation {
    if !device_name.starts_with("utun") {
        return observation(
            device_name,
            CapabilityState::Inactive,
            CapabilityState::Unknown,
            None,
        );
    }
    let device = device_state(read("/sbin/ifconfig", &[device_name]), device_name);
    let (route, interface) = match read("/sbin/route", &["-n", "get", "1.1.1.1"]) {
        Ok(output) if output.status.success() => (
            CapabilityState::Active,
            String::from_utf8(output.stdout)
                .ok()
                .and_then(|output| parse_value_after(&output, "interface:").map(str::to_owned)),
        ),
        Ok(_) => (CapabilityState::Inactive, None),
        Err(_) => (CapabilityState::Unknown, None),
    };
    observation(
        device_name,
        device,
        route_state(interface.as_deref(), device_name, route),
        interface.as_deref(),
    )
}

fn device_state(result: Result<Output, String>, device_name: &str) -> CapabilityState {
    let Ok(output) = result else {
        return CapabilityState::Unknown;
    };
    if !output.status.success() {
        return CapabilityState::Unknown;
    }
    let Ok(text) = std::str::from_utf8(&output.stdout) else {
        return CapabilityState::Unknown;
    };
    let Some(flags) = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(device_name))
        .and_then(|line| line.strip_prefix(": flags="))
        .and_then(|line| line.split_once('<'))
        .filter(|(bits, _)| u32::from_str_radix(bits, 16).is_ok())
        .and_then(|(_, tail)| tail.split_once('>'))
        .map(|(flags, _)| flags)
    else {
        return CapabilityState::Unknown;
    };
    if flags.split(',').any(|flag| flag == "UP") {
        CapabilityState::Active
    } else {
        CapabilityState::Inactive
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
