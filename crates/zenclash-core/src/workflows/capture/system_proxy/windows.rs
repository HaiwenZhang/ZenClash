#[cfg(any(target_os = "windows", test))]
use super::SystemProxyStatus;
#[cfg(target_os = "windows")]
use crate::MihomoResult;

#[cfg(any(target_os = "windows", test))]
mod connection_options;
#[cfg(any(target_os = "windows", test))]
mod snapshot;
#[cfg(target_os = "windows")]
pub(super) use snapshot::{WinInetProxySnapshot as ProxySnapshot, apply, capture, restore};

#[cfg(target_os = "windows")]
pub(super) fn detect() -> MihomoResult<String> {
    status("WinINET")?;
    Ok("WinINET".into())
}
#[cfg(target_os = "windows")]
pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    connection_options::default_connection_status(service, connection_options::query_default)
}
#[cfg(target_os = "windows")]
pub(super) fn set_enabled(
    service: &str,
    enabled: bool,
    host: &str,
    port: u16,
    bypass: &[String],
) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Manual { host, port, bypass }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}
#[cfg(target_os = "windows")]
pub(super) fn set_pac_enabled(service: &str, enabled: bool, url: &str) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Pac { url }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}

#[cfg(target_os = "windows")]
fn notify_wininet() -> MihomoResult<()> {
    use std::ptr;

    use windows_sys::Win32::Networking::WinInet::{
        INTERNET_OPTION_PROXY_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH,
        INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
    };

    for option in [
        INTERNET_OPTION_SETTINGS_CHANGED,
        INTERNET_OPTION_PROXY_SETTINGS_CHANGED,
        INTERNET_OPTION_REFRESH,
    ] {
        // SAFETY: Both options are bufferless; no borrowed data enters WinINET.
        // Notification rejection is an operation failure, retaining GetLastError.
        let succeeded = unsafe { InternetSetOptionW(ptr::null(), option, ptr::null(), 0) };
        if succeeded == 0 {
            return Err(crate::SystemProxyError::PlatformIo {
                backend: crate::SystemProxyBackendKind::WinInet,
                operation: "InternetSetOptionW",
                source: std::io::Error::last_os_error(),
            }
            .into());
        }
    }
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
        let server = server
            .strip_prefix('[')
            .and_then(|server| server.strip_suffix(']'))
            .unwrap_or(server);
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

fn parse_bypass(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_bypass, parse_proxy_server};

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
    fn parses_wininet_bypass_and_preserves_order() {
        assert_eq!(
            parse_bypass("localhost;127.*;*.example.com;<local>"),
            ["localhost", "127.*", "*.example.com", "<local>"]
        );
    }
}
