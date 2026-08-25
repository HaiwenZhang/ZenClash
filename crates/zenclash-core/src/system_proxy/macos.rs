use std::process::Output;

use super::SystemProxyStatus;
use crate::{MihomoError, MihomoResult};

pub(super) fn detect() -> MihomoResult<String> {
    if let Ok(service) = std::env::var("ZENCLASH_NETWORK_SERVICE") {
        if !service.trim().is_empty() {
            return Ok(service);
        }
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
        .ok_or_else(|| MihomoError::Process("未找到可用的 macOS 网络服务".into()))
}

pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    let web = run_networksetup(["-getwebproxy", service])?;
    let secure = run_networksetup(["-getsecurewebproxy", service])?;
    let (enabled, server, port) = parse_proxy_output(&web.stdout)?;
    let (secure_enabled, secure_server, secure_port) = parse_proxy_output(&secure.stdout)?;
    Ok(SystemProxyStatus {
        service: service.to_owned(),
        enabled,
        server,
        port,
        secure_enabled,
        secure_server,
        secure_port,
    })
}

fn active_network_service() -> Option<String> {
    let route = crate::platform_command::output("/sbin/route", &["-n", "get", "default"])
        .ok()
        .filter(|output| output.status.success())?;
    let interface = parse_default_interface(&route.stdout)?;
    let order = run_networksetup(["-listnetworkserviceorder"]).ok()?;
    parse_service_for_interface(&order.stdout, &interface)
}

pub(super) fn set_enabled(
    service: &str,
    enabled: bool,
    server: &str,
    port: u16,
) -> MihomoResult<()> {
    if !enabled {
        return set_proxy_states(service, false);
    }

    // Both native states are switched off before values change. If any write
    // fails, the proxy stays disabled instead of becoming partially active.
    set_proxy_states(service, false)?;
    let port = port.to_string();
    run_networksetup(["-setwebproxy", service, server, &port, "off"])?;
    run_networksetup(["-setsecurewebproxy", service, server, &port, "off"])?;
    run_networksetup([
        "-setproxybypassdomains",
        service,
        "localhost",
        "127.0.0.1",
        "::1",
        "*.local",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
    ])?;
    if let Err(error) = set_proxy_states(service, true) {
        let _ = set_proxy_states(service, false);
        return Err(error);
    }
    Ok(())
}

fn set_proxy_states(service: &str, enabled: bool) -> MihomoResult<()> {
    let state = if enabled { "on" } else { "off" };
    let web = run_networksetup(["-setwebproxystate", service, state]);
    let secure = run_networksetup(["-setsecurewebproxystate", service, state]);
    web.and(secure).map(|_| ())
}

fn run_networksetup<const N: usize>(args: [&str; N]) -> MihomoResult<Output> {
    let output = crate::platform_command::output("/usr/sbin/networksetup", &args)
        .map_err(MihomoError::Process)?;
    if output.status.success() {
        return Ok(output);
    }

    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(MihomoError::Process(if message.is_empty() {
        format!("networksetup 退出状态：{}", output.status)
    } else {
        message
    }))
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
        {
            if order.chars().all(|character| character.is_ascii_digit()) {
                service = Some(name.trim_start_matches('*').trim().to_owned());
                continue;
            }
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

fn parse_proxy_output(output: &[u8]) -> MihomoResult<(bool, String, u16)> {
    let output = String::from_utf8_lossy(output);
    let value = |key: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(key).map(str::trim))
            .ok_or_else(|| MihomoError::Process(format!("networksetup 输出缺少 {key}")))
    };
    let enabled = value("Enabled:")?.eq_ignore_ascii_case("yes");
    let server = value("Server:")?.to_owned();
    let port = value("Port:")?.parse().map_err(|error| {
        MihomoError::Process(format!("networksetup 返回了无效代理端口：{error}"))
    })?;
    Ok((enabled, server, port))
}

#[cfg(test)]
mod tests {
    use super::{parse_default_interface, parse_proxy_output, parse_service_for_interface};

    #[test]
    fn parses_networksetup_proxy_output() {
        let (enabled, server, port) = parse_proxy_output(
            b"Enabled: Yes\nServer: 127.0.0.1\nPort: 7890\nAuthenticated Proxy Enabled: 0\n",
        )
        .unwrap();
        assert!(enabled);
        assert_eq!(server, "127.0.0.1");
        assert_eq!(port, 7890);
    }

    #[test]
    fn rejects_invalid_networksetup_proxy_port() {
        let error =
            parse_proxy_output(b"Enabled: Yes\nServer: 127.0.0.1\nPort: invalid\n").unwrap_err();

        assert!(error.to_string().contains("无效代理端口"));
    }

    #[test]
    fn resolves_default_route_interface_to_network_service() {
        let route = b"route to: default\ninterface: en7\n";
        let order = b"An asterisk denotes that a network service is disabled.\n(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n(2) USB Ethernet\n(Hardware Port: USB Ethernet, Device: en7)\n";

        let interface = parse_default_interface(route).unwrap();

        assert_eq!(
            parse_service_for_interface(order, &interface).as_deref(),
            Some("USB Ethernet")
        );
    }
}
