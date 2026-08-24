use std::process::{Command, Output};

use crate::{MihomoError, MihomoResult};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemProxyStatus {
    pub service: String,
    pub enabled: bool,
    pub server: String,
    pub port: u16,
    pub secure_enabled: bool,
    pub secure_server: String,
    pub secure_port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxyManager {
    service: String,
}

impl SystemProxyManager {
    pub fn detect() -> MihomoResult<Self> {
        #[cfg(target_os = "macos")]
        {
            if let Ok(service) = std::env::var("ZENCLASH_NETWORK_SERVICE") {
                if !service.trim().is_empty() {
                    return Ok(Self { service });
                }
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
            let service = services
                .iter()
                .find(|service| service.as_str() == "Wi-Fi")
                .or_else(|| services.iter().find(|service| service.contains("Ethernet")))
                .or_else(|| services.first())
                .cloned()
                .ok_or_else(|| MihomoError::Process("未找到可用的 macOS 网络服务".into()))?;
            Ok(Self { service })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(MihomoError::Process("当前平台尚未实现系统代理控制".into()))
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn status(&self) -> MihomoResult<SystemProxyStatus> {
        #[cfg(target_os = "macos")]
        {
            let web = run_networksetup(["-getwebproxy", self.service.as_str()])?;
            let secure = run_networksetup(["-getsecurewebproxy", self.service.as_str()])?;
            let (enabled, server, port) = parse_proxy_output(&web.stdout);
            let (secure_enabled, secure_server, secure_port) = parse_proxy_output(&secure.stdout);
            Ok(SystemProxyStatus {
                service: self.service.clone(),
                enabled,
                server,
                port,
                secure_enabled,
                secure_server,
                secure_port,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(MihomoError::Process(
                "当前平台尚未实现系统代理状态读取".into(),
            ))
        }
    }

    pub fn set_enabled(&self, enabled: bool, server: &str, port: u16) -> MihomoResult<()> {
        #[cfg(target_os = "macos")]
        {
            if enabled {
                if server.trim().is_empty() || port == 0 {
                    return Err(MihomoError::Process("系统代理地址或端口无效".into()));
                }
                let port = port.to_string();
                run_networksetup([
                    "-setwebproxy",
                    self.service.as_str(),
                    server,
                    port.as_str(),
                    "off",
                ])?;
                run_networksetup([
                    "-setsecurewebproxy",
                    self.service.as_str(),
                    server,
                    port.as_str(),
                    "off",
                ])?;
                run_networksetup([
                    "-setproxybypassdomains",
                    self.service.as_str(),
                    "localhost",
                    "127.0.0.1",
                    "::1",
                    "*.local",
                    "192.168.0.0/16",
                    "10.0.0.0/8",
                    "172.16.0.0/12",
                ])?;
            }
            let state = if enabled { "on" } else { "off" };
            run_networksetup(["-setwebproxystate", self.service.as_str(), state])?;
            run_networksetup(["-setsecurewebproxystate", self.service.as_str(), state])?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (enabled, server, port);
            Err(MihomoError::Process("当前平台尚未实现系统代理设置".into()))
        }
    }
}

#[cfg(target_os = "macos")]
fn run_networksetup<const N: usize>(args: [&str; N]) -> MihomoResult<Output> {
    let output = Command::new("/usr/sbin/networksetup")
        .args(args)
        .output()
        .map_err(|error| MihomoError::Process(format!("执行 networksetup 失败：{error}")))?;
    if output.status.success() {
        Ok(output)
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(MihomoError::Process(if message.is_empty() {
            format!("networksetup 退出状态：{}", output.status)
        } else {
            message
        }))
    }
}

fn parse_proxy_output(output: &[u8]) -> (bool, String, u16) {
    let output = String::from_utf8_lossy(output);
    let value = |key: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(key).map(str::trim))
            .unwrap_or_default()
    };
    (
        value("Enabled:").eq_ignore_ascii_case("yes"),
        value("Server:").to_owned(),
        value("Port:").parse().unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_macos_networksetup_proxy_output() {
        let (enabled, server, port) = parse_proxy_output(
            b"Enabled: Yes\nServer: 127.0.0.1\nPort: 7890\nAuthenticated Proxy Enabled: 0\n",
        );
        assert!(enabled);
        assert_eq!(server, "127.0.0.1");
        assert_eq!(port, 7890);
    }
}
