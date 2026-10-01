#[cfg(target_os = "linux")]
use std::process::Output;

use super::SystemProxyStatus;
#[cfg(target_os = "linux")]
use super::command::run_checked;
use crate::{MihomoError, MihomoResult};

const PROXY_SCHEMA: &str = "org.gnome.system.proxy";

#[cfg(target_os = "linux")]
pub(super) fn detect() -> MihomoResult<String> {
    run_gsettings(["get", PROXY_SCHEMA, "mode"])?;
    Ok("GNOME".into())
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
pub(super) fn set_pac_enabled(_service: &str, enabled: bool, url: &str) -> MihomoResult<()> {
    run_gsettings(["set", PROXY_SCHEMA, "mode", "none"])?;
    if !enabled {
        return Ok(());
    }
    run_gsettings(["set", PROXY_SCHEMA, "autoconfig-url", url])?;
    run_gsettings(["set", PROXY_SCHEMA, "mode", "auto"])?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn restore_snapshot(previous: &SystemProxyStatus) -> MihomoResult<()> {
    for command in snapshot_commands(previous)? {
        let args = command.iter().map(String::as_str).collect::<Vec<_>>();
        run_checked("gsettings", &args)?;
    }
    Ok(())
}

fn snapshot_commands(previous: &SystemProxyStatus) -> MihomoResult<Vec<[String; 4]>> {
    if previous.enabled != previous.secure_enabled || (previous.auto_enabled && previous.enabled) {
        return Err(MihomoError::Process(zenclash_i18n::text(
            "system_proxy.errors.verification",
        )));
    }
    let mode = if previous.auto_enabled {
        "auto"
    } else if previous.enabled {
        "manual"
    } else {
        "none"
    };
    let commands = [
        (PROXY_SCHEMA, "mode", "none".into()),
        (
            "org.gnome.system.proxy.http",
            "host",
            previous.server.clone(),
        ),
        (
            "org.gnome.system.proxy.http",
            "port",
            previous.port.to_string(),
        ),
        (
            "org.gnome.system.proxy.https",
            "host",
            previous.secure_server.clone(),
        ),
        (
            "org.gnome.system.proxy.https",
            "port",
            previous.secure_port.to_string(),
        ),
        (
            PROXY_SCHEMA,
            "ignore-hosts",
            format_gsettings_list(&previous.bypass),
        ),
        (PROXY_SCHEMA, "autoconfig-url", previous.auto_url.clone()),
        (PROXY_SCHEMA, "mode", mode.into()),
    ];
    Ok(commands
        .into_iter()
        .map(|(schema, key, value)| ["set".into(), schema.into(), key.into(), value])
        .collect())
}

#[cfg(target_os = "linux")]
fn run_gsettings<const N: usize>(args: [&str; N]) -> MihomoResult<Output> {
    run_checked("gsettings", &args)
}

#[cfg(target_os = "linux")]
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
    use super::{
        SystemProxyStatus, format_gsettings_list, parse_gsettings_list, parse_proxy_port,
        snapshot_commands,
    };

    #[test]
    fn restore_plan_preserves_distinct_cached_endpoints_url_bypass_and_final_mode() {
        for mode in ["none", "manual", "auto"] {
            let previous = SystemProxyStatus {
                enabled: mode == "manual",
                secure_enabled: mode == "manual",
                server: "http.original.test".into(),
                port: 8080,
                secure_server: "https.original.test".into(),
                secure_port: 8443,
                auto_enabled: mode == "auto",
                auto_url: "http://pac.original.test/pac".into(),
                bypass: vec!["localhost".into(), "*.original.test".into()],
                ..Default::default()
            };
            let commands = snapshot_commands(&previous).unwrap();
            let value = |schema: &str, key: &str| {
                commands
                    .iter()
                    .rev()
                    .find(|command| command[1] == schema && command[2] == key)
                    .unwrap()[3]
                    .clone()
            };
            assert_eq!(
                value("org.gnome.system.proxy.http", "host"),
                previous.server
            );
            assert_eq!(value("org.gnome.system.proxy.http", "port"), "8080");
            assert_eq!(
                value("org.gnome.system.proxy.https", "host"),
                previous.secure_server
            );
            assert_eq!(value("org.gnome.system.proxy.https", "port"), "8443");
            assert_eq!(
                value("org.gnome.system.proxy", "autoconfig-url"),
                previous.auto_url
            );
            assert_eq!(
                parse_gsettings_list(&value("org.gnome.system.proxy", "ignore-hosts")),
                previous.bypass
            );
            assert_eq!(commands.first().unwrap()[3], "none");
            assert_eq!(commands.last().unwrap()[3], mode);
        }
    }

    #[test]
    fn restore_plan_accepts_empty_disabled_endpoints_and_rejects_unrepresentable_flags() {
        let mut previous = SystemProxyStatus::default();
        let commands = snapshot_commands(&previous).unwrap();
        assert_eq!(commands[1][3], "");
        assert_eq!(commands[2][3], "0");
        assert_eq!(commands[3][3], "");
        assert_eq!(commands[4][3], "0");
        previous.enabled = true;
        assert!(snapshot_commands(&previous).is_err());
    }

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
