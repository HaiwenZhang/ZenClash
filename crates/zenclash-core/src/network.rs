use std::process::Command;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemNetworkSnapshot {
    pub interface: String,
    pub gateway: String,
    pub local_ipv4: String,
    pub dns_servers: Vec<String>,
    pub error: Option<String>,
}

impl SystemNetworkSnapshot {
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            match detect_macos() {
                Ok(snapshot) => snapshot,
                Err(error) => Self {
                    error: Some(error),
                    ..Default::default()
                },
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                error: Some("当前平台尚未实现网络接口探测".into()),
                ..Default::default()
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn detect_macos() -> Result<SystemNetworkSnapshot, String> {
    let route = command_output("/sbin/route", &["-n", "get", "default"])?;
    let interface = route_value(&route, "interface:");
    let gateway = route_value(&route, "gateway:");
    let local_ipv4 = if interface.is_empty() {
        String::new()
    } else {
        command_output("/usr/sbin/ipconfig", &["getifaddr", &interface])
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let dns = command_output("/usr/sbin/scutil", &["--dns"]).unwrap_or_default();
    let mut dns_servers = dns
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("nameserver[")
                .and_then(|line| line.split_once(": "))
                .map(|(_, value)| value.trim().to_owned())
        })
        .collect::<Vec<_>>();
    dns_servers.sort();
    dns_servers.dedup();
    Ok(SystemNetworkSnapshot {
        interface,
        gateway,
        local_ipv4,
        dns_servers,
        error: None,
    })
}

#[cfg(target_os = "macos")]
fn command_output(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|error| format!("执行 {command} 失败：{error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn route_value(output: &str, key: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(key).map(str::trim))
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_route_fields() {
        let output = "   route to: default\ndestination: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(route_value(output, "gateway:"), "192.168.1.1");
        assert_eq!(route_value(output, "interface:"), "en0");
    }
}
