#[cfg(target_os = "windows")]
use super::command::run_checked;
#[cfg(target_os = "windows")]
use super::SystemProxyStatus;
#[cfg(target_os = "windows")]
use crate::{MihomoError, MihomoResult};

#[cfg(target_os = "windows")]
const INTERNET_SETTINGS: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

#[cfg(any(target_os = "windows", test))]
const PROXY_OVERRIDE: &str = "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>";

#[cfg(target_os = "windows")]
pub(super) fn detect() -> MihomoResult<String> {
    query_value("ProxyEnable")?;
    Ok("WinINET".into())
}

#[cfg(target_os = "windows")]
pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    let enabled = query_value("ProxyEnable")?
        .split_whitespace()
        .last()
        .is_some_and(|value| value == "0x1" || value == "1");
    let server_output = match query_value("ProxyServer") {
        Ok(output) => output,
        Err(_) if !enabled => String::new(),
        Err(error) => return Err(error),
    };
    let value = server_output
        .lines()
        .find_map(|line| line.split_once("REG_SZ").map(|(_, value)| value.trim()))
        .unwrap_or_default();
    let (server, port, secure_server, secure_port) =
        parse_proxy_server(value).map_err(MihomoError::Process)?;
    Ok(SystemProxyStatus {
        service: service.to_owned(),
        enabled: enabled && !server.is_empty() && port > 0,
        server,
        port,
        secure_enabled: enabled && !secure_server.is_empty() && secure_port > 0,
        secure_server,
        secure_port,
    })
}

#[cfg(target_os = "windows")]
pub(super) fn set_enabled(
    _service: &str,
    enabled: bool,
    server: &str,
    port: u16,
) -> MihomoResult<()> {
    let update = update_registry(enabled, server, port);
    let notification = notify_wininet();
    combine_update_and_notification(update, notification)
}

#[cfg(target_os = "windows")]
fn update_registry(enabled: bool, server: &str, port: u16) -> MihomoResult<()> {
    if !enabled {
        return set_proxy_enabled(false);
    }

    // ProxyEnable is cleared before updating an existing WinINET proxy. It is
    // restored only after both the endpoint and bypass list are complete.
    set_proxy_enabled(false)?;
    let proxy = format!("http={server}:{port};https={server}:{port}");
    add_value("ProxyServer", "REG_SZ", &proxy)?;
    add_value("ProxyOverride", "REG_SZ", PROXY_OVERRIDE)?;
    set_proxy_enabled(true)
}

#[cfg(target_os = "windows")]
fn notify_wininet() -> MihomoResult<()> {
    use std::ptr;

    use windows_sys::Win32::Networking::WinInet::{
        InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
    };

    for option in [INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH] {
        // SAFETY: A null internet handle applies the option globally. Both
        // options take no buffer, as required by the WinINet API contract.
        let succeeded = unsafe { InternetSetOptionW(ptr::null(), option, ptr::null(), 0) };
        if succeeded == 0 {
            return Err(MihomoError::Process(format!(
                "通知 WinINET 刷新代理设置失败：{}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn combine_update_and_notification(
    update: MihomoResult<()>,
    notification: MihomoResult<()>,
) -> MihomoResult<()> {
    match (update, notification) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(update), Err(notification)) => Err(MihomoError::Process(format!(
            "更新系统代理失败：{update}；刷新 WinINET 设置也失败：{notification}"
        ))),
    }
}

#[cfg(target_os = "windows")]
fn set_proxy_enabled(enabled: bool) -> MihomoResult<()> {
    add_value("ProxyEnable", "REG_DWORD", if enabled { "1" } else { "0" })
}

#[cfg(target_os = "windows")]
fn query_value(name: &str) -> MihomoResult<String> {
    let output = run_checked("reg.exe", &["query", INTERNET_SETTINGS, "/v", name])?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "windows")]
fn add_value(name: &str, value_type: &str, value: &str) -> MihomoResult<()> {
    run_checked(
        "reg.exe",
        &[
            "add",
            INTERNET_SETTINGS,
            "/v",
            name,
            "/t",
            value_type,
            "/d",
            value,
            "/f",
        ],
    )?;
    Ok(())
}

fn parse_proxy_server(value: &str) -> Result<(String, u16, String, u16), String> {
    let parse_endpoint = |value: &str| {
        let (server, port) = value
            .rsplit_once(':')
            .ok_or_else(|| format!("WinINET 代理地址缺少端口：{value}"))?;
        if server.trim().is_empty() {
            return Err("WinINET 代理服务器为空".to_owned());
        }
        let port = port
            .parse::<u16>()
            .map_err(|error| format!("WinINET 代理端口无效“{port}”：{error}"))?;
        if port == 0 {
            return Err("WinINET 代理端口不能为 0".to_owned());
        }
        Ok((server.to_owned(), port))
    };
    if value.trim().is_empty() {
        return Ok((String::new(), 0, String::new(), 0));
    }
    if !value.contains('=') {
        let (server, port) = parse_endpoint(value)?;
        return Ok((server.clone(), port, server, port));
    }
    let mut http = (String::new(), 0);
    let mut https = (String::new(), 0);
    for entry in value.split(';').map(str::trim) {
        let Some((scheme, endpoint)) = entry.split_once('=') else {
            continue;
        };
        if scheme.eq_ignore_ascii_case("http") {
            http = parse_endpoint(endpoint.trim())?;
        } else if scheme.eq_ignore_ascii_case("https") {
            https = parse_endpoint(endpoint.trim())?;
        }
    }
    Ok((http.0, http.1, https.0, https.1))
}

#[cfg(test)]
mod tests {
    use super::{parse_proxy_server, PROXY_OVERRIDE};

    #[test]
    fn parses_protocol_specific_proxy_server() {
        assert_eq!(
            parse_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7891"),
            Ok(("127.0.0.1".to_owned(), 7890, "127.0.0.1".to_owned(), 7891))
        );
    }

    #[test]
    fn applies_single_proxy_to_both_protocols() {
        assert_eq!(
            parse_proxy_server("proxy.lan:8080"),
            Ok(("proxy.lan".to_owned(), 8080, "proxy.lan".to_owned(), 8080))
        );
    }

    #[test]
    fn rejects_proxy_server_with_invalid_port() {
        let error = parse_proxy_server("http=127.0.0.1:not-a-port").unwrap_err();

        assert!(error.contains("端口无效"));
    }

    #[test]
    fn parses_protocol_names_case_insensitively() {
        assert_eq!(
            parse_proxy_server("HTTP=proxy.lan:8080; HTTPS=secure.lan:8443"),
            Ok(("proxy.lan".to_owned(), 8080, "secure.lan".to_owned(), 8443))
        );
    }

    #[test]
    fn rejects_zero_proxy_port() {
        let error = parse_proxy_server("proxy.lan:0").unwrap_err();

        assert!(error.contains("不能为 0"));
    }

    #[test]
    fn bypasses_the_full_private_172_16_network() {
        assert!(PROXY_OVERRIDE.contains("172.16.*") && PROXY_OVERRIDE.contains("172.31.*"));
    }
}
