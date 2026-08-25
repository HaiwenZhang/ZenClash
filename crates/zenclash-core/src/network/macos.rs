#[cfg(any(target_os = "macos", test))]
use super::unique_nonempty;
#[cfg(target_os = "macos")]
use super::{command, warning_message, SystemNetworkSnapshot};

#[cfg(target_os = "macos")]
pub(super) fn detect() -> Result<SystemNetworkSnapshot, String> {
    let route = command::output("/sbin/route", &["-n", "get", "default"])?;
    let interface = route_value(&route, "interface:");
    if interface.is_empty() {
        return Err("无法从 macOS 默认路由确定网络接口".into());
    }
    let gateway = route_value(&route, "gateway:");
    let mut warnings = Vec::new();
    let local_ipv4 = match command::output("/usr/sbin/ipconfig", &["getifaddr", &interface]) {
        Ok(address) => address.trim().to_owned(),
        Err(error) => {
            warnings.push(format!("读取接口 IPv4 地址失败：{error}"));
            String::new()
        }
    };
    let dns_servers = match command::output("/usr/sbin/scutil", &["--dns"]) {
        Ok(dns) => parse_scutil_dns(&dns),
        Err(error) => {
            warnings.push(format!("读取系统 DNS 失败：{error}"));
            Vec::new()
        }
    };
    Ok(SystemNetworkSnapshot {
        interface,
        gateway,
        local_ipv4,
        dns_servers,
        error: warning_message(&warnings),
    })
}

fn route_value(output: &str, key: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(key).map(str::trim))
        .unwrap_or_default()
        .to_owned()
}

fn parse_scutil_dns(output: &str) -> Vec<String> {
    unique_nonempty(output.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("nameserver[")
            .and_then(|line| line.split_once(": "))
            .map(|(_, value)| value.trim().to_owned())
    }))
}

#[cfg(test)]
mod tests {
    use super::{parse_scutil_dns, route_value};

    #[test]
    fn route_value_reads_macos_default_gateway() {
        let route = "gateway: 192.168.1.1\ninterface: en0\n";
        assert_eq!(route_value(route, "gateway:"), "192.168.1.1");
    }

    #[test]
    fn parse_scutil_dns_preserves_priority_order_and_deduplicates_servers() {
        let dns = "nameserver[0] : 8.8.8.8\nnameserver[1] : 1.1.1.1\nnameserver[0] : 8.8.8.8\n";
        assert_eq!(
            parse_scutil_dns(dns),
            vec!["8.8.8.8".to_owned(), "1.1.1.1".to_owned()]
        );
    }
}
