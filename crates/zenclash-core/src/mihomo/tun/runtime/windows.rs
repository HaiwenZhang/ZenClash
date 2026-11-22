use std::process::{Command, Output};
#[cfg(target_os = "windows")]
use std::time::Duration;

use serde::Deserialize;

use super::*;

pub(super) const DEVICE_ENV_VAR: &str = "ZENCLASH_TUN_DEVICE";
const POWERSHELL_PROGRAM: &str = "powershell.exe";
#[cfg(target_os = "windows")]
const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) const SCRIPT: &str = concat!(
    "$ErrorActionPreference='Stop'; ",
    "$name=$env:ZENCLASH_TUN_DEVICE; ",
    "if ([string]::IsNullOrWhiteSpace($name)) { throw 'Missing TUN device name' }; ",
    "$adapter=Get-NetAdapter -Name $name -IncludeHidden -ErrorAction SilentlyContinue | Select-Object -First 1; ",
    "$route=Find-NetRoute -RemoteIPAddress '1.1.1.1' | Where-Object { $_.PSObject.Properties.Name -contains 'DestinationPrefix' } | Select-Object -First 1; ",
    "[pscustomobject]@{deviceStatus=if($null -eq $adapter){$null}else{$adapter.Status.ToString()};routeAlias=if($null -eq $route){$null}else{$route.InterfaceAlias}} | ConvertTo-Json -Compress"
);

const POWERSHELL_ARGS: [&str; 4] = ["-NoProfile", "-NonInteractive", "-Command", SCRIPT];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsObservation {
    device_status: Option<String>,
    route_alias: Option<String>,
}

fn build_command(device_name: &str) -> Command {
    let mut command = Command::new(POWERSHELL_PROGRAM);
    command
        .args(POWERSHELL_ARGS)
        .env(DEVICE_ENV_VAR, device_name);
    command
}

#[cfg(target_os = "windows")]
pub(super) fn observe(device_name: &str) -> TunRuntimeObservation {
    observe_with(device_name, |command| {
        crate::platform::command::output_from_command(
            command,
            POWERSHELL_PROGRAM,
            POWERSHELL_TIMEOUT,
        )
    })
}

pub(super) fn observe_with(
    device_name: &str,
    mut read: impl FnMut(Command) -> Result<Output, String>,
) -> TunRuntimeObservation {
    if !is_valid_device_name(device_name) {
        return TunRuntimeObservation::invalid_device();
    }
    let result = read(build_command(device_name));
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

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::test_support::tun::output;

    #[test]
    fn powershell_command_passes_device_via_env_var_and_never_in_command_args() {
        assert!(SCRIPT.contains("$env:ZENCLASH_TUN_DEVICE"));
        assert!(!SCRIPT.contains("$args"));

        let mut invoked = false;
        let observed = observe_with("ZenClash-TUN", |command| {
            invoked = true;
            assert_eq!(command.get_program(), OsStr::new("powershell.exe"));

            let args: Vec<&OsStr> = command.get_args().collect();
            assert_eq!(
                args,
                [
                    OsStr::new("-NoProfile"),
                    OsStr::new("-NonInteractive"),
                    OsStr::new("-Command"),
                    OsStr::new(SCRIPT),
                ]
            );
            assert!(
                !args
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains("ZenClash-TUN")),
                "device name must never be interpolated or appended to -Command args"
            );

            assert!(command.get_envs().any(|entry| {
                entry == (OsStr::new(DEVICE_ENV_VAR), Some(OsStr::new("ZenClash-TUN")))
            }));

            Ok(output(
                br#"{"deviceStatus":"Up","routeAlias":"ZenClash-TUN"}"#,
                true,
            ))
        });

        assert!(invoked);
        assert_eq!(observed.device_name.as_deref(), Some("ZenClash-TUN"));
        assert_eq!(observed.device, CapabilityState::Active);
        assert_eq!(observed.route, CapabilityState::Active);
    }

    #[test]
    fn invalid_device_names_never_spawn_powershell() {
        for payload in [
            "",
            "   ",
            "-NoProfile",
            "Mihomo; Start-Process calc #",
            "Mihomo | whoami",
            "Mihomo & calc",
            "$(whoami)",
            "Mihomo`nwhoami",
            "Mihomo' ; calc",
            "Mihomo\" ; calc",
            "Mihomo\0",
            "Mihomo\ncalc",
            "*",
        ] {
            let observed = observe_with(payload, |_| {
                panic!("invalid device name {payload:?} must not spawn PowerShell")
            });
            assert_eq!(observed.device, CapabilityState::Unknown);
            assert_eq!(observed.route, CapabilityState::Unknown);
            assert!(observed.device_name.is_none());
        }
    }

    #[test]
    fn down_adapter_and_failed_or_malformed_output_are_classified_safely() {
        let down = observe_with("Mihomo", |_| {
            Ok(output(
                br#"{"deviceStatus":"Disconnected","routeAlias":"Ethernet"}"#,
                true,
            ))
        });
        assert_eq!(down.device, CapabilityState::Inactive);
        assert_eq!(down.route, CapabilityState::Inactive);

        for reply in [
            Err("spawn failed".into()),
            Ok(output(
                br#"{"deviceStatus":"Up","routeAlias":"Mihomo"}"#,
                false,
            )),
            Ok(output(b"not-json", true)),
        ] {
            let mut reply = Some(reply);
            let observed = observe_with("Mihomo", |_| reply.take().unwrap());
            assert_eq!(observed.device, CapabilityState::Unknown);
            assert_eq!(observed.route, CapabilityState::Unknown);
            assert_eq!(observed.device_name.as_deref(), Some("Mihomo"));
        }
    }
}
