use std::process::Output;

use super::{command::run_checked, SystemProxyStatus};
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
    let enabled = mode == "manual";
    Ok(SystemProxyStatus {
        service: service.to_owned(),
        enabled,
        server,
        port,
        secure_enabled: enabled,
        secure_server,
        secure_port,
    })
}

pub(super) fn set_enabled(
    _service: &str,
    enabled: bool,
    server: &str,
    port: u16,
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
    run_gsettings([
        "set",
        PROXY_SCHEMA,
        "ignore-hosts",
        "['localhost', '127.0.0.0/8', '::1', '*.local', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16']",
    ])?;
    run_gsettings(["set", PROXY_SCHEMA, "mode", "manual"])?;
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

#[cfg(test)]
mod tests {
    use super::parse_proxy_port;

    #[test]
    fn rejects_invalid_gsettings_proxy_port() {
        let error = parse_proxy_port("not-a-port").unwrap_err();

        assert!(error.to_string().contains("无效代理端口"));
    }
}
