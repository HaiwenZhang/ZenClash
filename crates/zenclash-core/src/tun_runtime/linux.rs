use std::process::Output;

use super::*;

#[cfg(target_os = "linux")]
pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
    observe_with(device_name, crate::platform_command::output)
}

pub(super) fn observe_with(
    device_name: &str,
    mut read: impl FnMut(&str, &[&str]) -> Result<Output, String>,
) -> TunRuntimeObservation {
    let device = device_state(
        read("ip", &["link", "show", "dev", device_name]),
        device_name,
    );
    let interface = match read("ip", &["-4", "route", "get", "1.1.1.1"]) {
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
        .and_then(|line| line.split_once(':'))
        .filter(|(index, _)| index.parse::<u32>().is_ok())
        .and_then(|(_, rest)| rest.trim_start().split_once(": "))
        .filter(|(name, _)| name.split('@').next() == Some(device_name))
        .and_then(|(_, rest)| rest.strip_prefix('<'))
        .and_then(|tail| tail.split_once('>'))
        .map(|(flags, _)| flags)
    else {
        return CapabilityState::Unknown;
    };
    // Virtual devices need not expose carrier or an operational state of UP.
    if flags.split(',').any(|flag| flag == "UP") {
        CapabilityState::Active
    } else {
        CapabilityState::Inactive
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
