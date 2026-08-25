#[cfg(any(target_os = "linux", test))]
use super::unique_nonempty;
#[cfg(target_os = "linux")]
use super::{command, warning_message, SystemNetworkSnapshot};

#[cfg(target_os = "linux")]
pub(super) fn detect() -> Result<SystemNetworkSnapshot, String> {
    let route = command::output("ip", &["route", "show", "default"])?;
    let interface = token_after(&route, "dev");
    if interface.is_empty() {
        return Err("无法从默认路由确定网络接口".into());
    }
    let gateway = token_after(&route, "via");
    let mut warnings = Vec::new();
    let local_ipv4 = match command::output("ip", &["-o", "-4", "addr", "show", "dev", &interface]) {
        Ok(addresses) => token_after(&addresses, "inet")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned(),
        Err(error) => {
            warnings.push(format!("读取接口 IPv4 地址失败：{error}"));
            String::new()
        }
    };
    let dns_servers = match std::fs::read_to_string("/etc/resolv.conf") {
        Ok(contents) => parse_resolv_conf(&contents),
        Err(error) => {
            warnings.push(format!("读取 /etc/resolv.conf 失败：{error}"));
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

fn token_after(output: &str, token: &str) -> String {
    let mut fields = output.split_whitespace();
    while let Some(field) = fields.next() {
        if field == token {
            return fields.next().unwrap_or_default().to_owned();
        }
    }
    String::new()
}

fn parse_resolv_conf(contents: &str) -> Vec<String> {
    unique_nonempty(contents.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("nameserver") {
            fields.next().map(str::to_owned)
        } else {
            None
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{parse_resolv_conf, token_after};

    #[test]
    fn token_after_reads_default_route_interface() {
        let route = "default via 192.168.1.1 dev enp0s3 proto dhcp";
        assert_eq!(token_after(route, "dev"), "enp0s3");
    }

    #[test]
    fn parse_resolv_conf_reads_unique_nameservers_in_priority_order() {
        let resolv =
            "# generated\nsearch lan\nnameserver 1.1.1.1\nnameserver 8.8.8.8\nnameserver 1.1.1.1\n";
        assert_eq!(
            parse_resolv_conf(resolv),
            vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()]
        );
    }
}
