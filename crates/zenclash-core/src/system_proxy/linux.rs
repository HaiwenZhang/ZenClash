use std::process::Output;

use super::{SystemProxyStatus, command::run_checked};
use crate::{MihomoError, MihomoResult};

const PROXY_SCHEMA: &str = "org.gnome.system.proxy";

pub(super) fn detect() -> MihomoResult<String> {
    run_gsettings(["get", PROXY_SCHEMA, "mode"])?;
    Ok("GNOME".into())
}

pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    let mode = gsettings_value(["get", PROXY_SCHEMA, "mode"])?;
    let server = gsettings_value(["get", "org.gnome.system.proxy.http", "host"])?;
    let port = parse_proxy_port(&gsettings_value([
        "get",
        "org.gnome.system.proxy.http",
        "port",
    ])?)?;
    let secure_server = gsettings_value(["get", "org.gnome.system.proxy.https", "host"])?;
    let secure_port = parse_proxy_port(&gsettings_value([
        "get",
        "org.gnome.system.proxy.https",
        "port",
    ])?)?;
    let bypass = parse_gsettings_list(&gsettings_value(["get", PROXY_SCHEMA, "ignore-hosts"])?);
    let auto_url = gsettings_value(["get", PROXY_SCHEMA, "autoconfig-url"])?;
    let enabled = mode == "manual";
    Ok(SystemProxyStatus {
        service: service.to_owned(),
        enabled,
        server,
        port,
        secure_enabled: enabled,
        secure_server,
        secure_port,
        bypass,
        auto_enabled: mode == "auto" && !auto_url.is_empty(),
        auto_url,
    })
}

pub(super) fn set_enabled(
    _service: &str,
    enabled: bool,
    server: &str,
    port: u16,
    bypass: &[String],
) -> MihomoResult<()> {
    if !enabled {
        run_gsettings(["set", PROXY_SCHEMA, "mode", "none"])?;
        return Ok(());
    }

    // Disable first: updating an already active GNOME proxy must not expose
    // only half of the new HTTP/HTTPS configuration.
    run_gsettings(["set", PROXY_SCHEMA, "mode", "none"])?;
    let port = port.to_string();
    run_gsettings(["set", "org.gnome.system.proxy.http", "host", server])?;
    run_gsettings(["set", "org.gnome.system.proxy.http", "port", &port])?;
    run_gsettings(["set", "org.gnome.system.proxy.https", "host", server])?;
    run_gsettings(["set", "org.gnome.system.proxy.https", "port", &port])?;
    let bypass = format_gsettings_list(bypass);
    run_gsettings(["set", PROXY_SCHEMA, "ignore-hosts", &bypass])?;
    run_gsettings(["set", PROXY_SCHEMA, "mode", "manual"])?;
    Ok(())
}

pub(super) fn set_pac_enabled(_service: &str, enabled: bool, url: &str) -> MihomoResult<()> {
    run_gsettings(["set", PROXY_SCHEMA, "mode", "none"])?;
    if !enabled {
        return Ok(());
    }
    run_gsettings(["set", PROXY_SCHEMA, "autoconfig-url", url])?;
    run_gsettings(["set", PROXY_SCHEMA, "mode", "auto"])?;
    Ok(())
}

fn run_gsettings<const N: usize>(args: [&str; N]) -> MihomoResult<Output> {
    run_checked("gsettings", &args)
}

fn gsettings_value<const N: usize>(args: [&str; N]) -> MihomoResult<String> {
    let output = run_gsettings(args)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_matches('\'')
        .to_owned())
}

fn parse_proxy_port(value: &str) -> MihomoResult<u16> {
    value.parse().map_err(|error| {
        MihomoError::Process(format!("gsettings 返回了无效代理端口“{value}”：{error}"))
    })
}

fn format_gsettings_list(entries: &[String]) -> String {
    format!(
        "[{}]",
        entries
            .iter()
            .map(|entry| format!("'{entry}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_gsettings_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().trim_matches('\''))
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{format_gsettings_list, parse_gsettings_list, parse_proxy_port};

    #[test]
    fn rejects_invalid_gsettings_proxy_port() {
        let error = parse_proxy_port("not-a-port").unwrap_err();

        assert!(error.to_string().contains("无效代理端口"));
    }

    #[test]
    fn gsettings_bypass_list_round_trips_in_order() {
        let entries = vec![
            "localhost".into(),
            "192.168.0.0/16".into(),
            "*.local".into(),
        ];

        assert_eq!(
            parse_gsettings_list(&format_gsettings_list(&entries)),
            entries
        );
    }
}
