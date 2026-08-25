use serde::Deserialize;

use super::{unique_nonempty, SystemNetworkSnapshot};

#[cfg(target_os = "windows")]
const NETWORK_SCRIPT: &str = "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $route=Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric,InterfaceMetric | Select-Object -First 1; if($null -eq $route){throw 'No default IPv4 route'}; $adapter=Get-NetAdapter -InterfaceIndex $route.InterfaceIndex; $ip=(Get-NetIPAddress -AddressFamily IPv4 -InterfaceIndex $route.InterfaceIndex | Where-Object {$_.IPAddress -notlike '169.254*'} | Select-Object -First 1).IPAddress; $dns=@((Get-DnsClientServerAddress -AddressFamily IPv4 -InterfaceIndex $route.InterfaceIndex).ServerAddresses); [pscustomobject]@{interface=$adapter.Name;gateway=$route.NextHop;local_ipv4=$ip;dns_servers=$dns} | ConvertTo-Json -Compress";

#[derive(Deserialize, Default)]
#[serde(default)]
struct WindowsSnapshot {
    interface: Option<String>,
    gateway: Option<String>,
    local_ipv4: Option<String>,
    dns_servers: Option<Vec<String>>,
}

#[cfg(target_os = "windows")]
pub(super) fn detect() -> Result<SystemNetworkSnapshot, String> {
    let output = crate::platform_command::output(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", NETWORK_SCRIPT],
    )
    .map_err(|error| format!("执行 PowerShell 网络探测失败：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("PowerShell 网络探测退出状态：{}", output.status)
        } else {
            stderr
        });
    }
    parse_snapshot(&output.stdout)
}

fn parse_snapshot(payload: &[u8]) -> Result<SystemNetworkSnapshot, String> {
    let snapshot: WindowsSnapshot = serde_json::from_slice(payload)
        .map_err(|error| format!("解析 Windows 网络信息失败：{error}"))?;
    let interface = snapshot.interface.unwrap_or_default();
    if interface.trim().is_empty() {
        return Err("Windows 网络探测未返回活动适配器".into());
    }
    Ok(SystemNetworkSnapshot {
        interface,
        gateway: snapshot.gateway.unwrap_or_default(),
        local_ipv4: snapshot.local_ipv4.unwrap_or_default(),
        dns_servers: unique_nonempty(snapshot.dns_servers.unwrap_or_default()),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_snapshot;

    #[test]
    fn parse_snapshot_accepts_missing_optional_windows_fields() {
        let snapshot = parse_snapshot(br#"{"interface":"Ethernet"}"#).unwrap();
        assert_eq!(
            (
                snapshot.interface,
                snapshot.local_ipv4,
                snapshot.dns_servers
            ),
            ("Ethernet".to_owned(), String::new(), Vec::new())
        );
    }

    #[test]
    fn parse_snapshot_rejects_missing_windows_adapter() {
        assert!(parse_snapshot(br#"{"gateway":"192.168.1.1"}"#).is_err());
    }

    #[test]
    fn parse_snapshot_accepts_null_windows_address_and_dns() {
        let snapshot =
            parse_snapshot(br#"{"interface":"Ethernet","local_ipv4":null,"dns_servers":null}"#)
                .unwrap();
        assert_eq!(
            (snapshot.local_ipv4, snapshot.dns_servers),
            (String::new(), Vec::new())
        );
    }

    #[test]
    fn parse_snapshot_preserves_unicode_adapter_names_from_utf8_powershell() {
        let snapshot = parse_snapshot("{\"interface\":\"以太网\"}".as_bytes()).unwrap();

        assert_eq!(snapshot.interface, "以太网");
    }

    #[test]
    fn parse_snapshot_preserves_dns_priority_order_and_deduplicates() {
        let snapshot = parse_snapshot(
            br#"{"interface":"Ethernet","dns_servers":["8.8.8.8","1.1.1.1","8.8.8.8"]}"#,
        )
        .unwrap();

        assert_eq!(
            snapshot.dns_servers,
            ["8.8.8.8".to_owned(), "1.1.1.1".to_owned()]
        );
    }
}
