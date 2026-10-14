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
