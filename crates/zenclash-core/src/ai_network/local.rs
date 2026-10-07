use super::{CheckResult, Status, message, tr};
use crate::{SystemNetworkSnapshot, SystemProxyManager};
use serde_json::{Value, json};
use std::{env, fs, io::Read, path::PathBuf};

pub(super) fn proxy(tun: Option<bool>) -> CheckResult {
    let system = SystemProxyManager::detect()
        .and_then(|manager| manager.status())
        .ok();
    let enabled = system
        .as_ref()
        .map(|status| status.enabled || status.secure_enabled || status.auto_enabled);
    let variables: Vec<_> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .filter(|key| env::var(key).is_ok_and(|value| !value.trim().is_empty()))
    .collect();
    CheckResult::new(
        if enabled.is_none() || tun.is_none() {
            Status::Unknown
        } else {
            Status::Info
        },
        message(
            "proxy_result",
            &[("system", switch(enabled)), ("tun", switch(tun))],
        ),
        message(
            "proxy_detail",
            &[(
                "env",
                if variables.is_empty() {
                    tr("unset")
                } else {
                    variables.join(", ")
                },
            )],
        ),
        json!({"system_proxy_enabled":enabled,"mihomo_tun_configured":tun,"environment_variable_names":variables}),
    )
}

fn switch(value: Option<bool>) -> String {
    tr(match value {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "unknown",
    })
}

pub(super) fn dns() -> CheckResult {
    let snapshot = SystemNetworkSnapshot::detect();
    dns_result(&snapshot.dns_servers, snapshot.error.is_some())
}

pub(super) fn dns_result(servers: &[String], incomplete: bool) -> CheckResult {
    let known_domestic = [
        "223.5.5.5",
        "223.6.6.6",
        "119.29.29.29",
        "114.114.114.114",
        "114.114.115.115",
        "180.76.76.76",
    ];
    let domestic = servers
        .iter()
        .any(|server| known_domestic.contains(&server.as_str()));
    CheckResult::new(
        if domestic {
            Status::Attention
        } else if incomplete || servers.is_empty() {
            Status::Unknown
        } else {
            Status::Info
        },
        if servers.is_empty() {
            tr("dns_missing")
        } else {
            servers.join(" · ")
        },
        tr(if domestic {
            "dns_domestic"
        } else {
            "dns_observation"
        }),
        json!({"system_dns":servers, "known_domestic_match":domestic, "partial_discovery":incomplete}),
    )
}

pub(super) fn timezone(exit: &Value) -> CheckResult {
    // iana-time-zone reads the OS setting rather than TZ (unlike chrono::Local).
    let system = iana_time_zone::get_timezone().ok();
    let cli = env::var("TZ")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| system.clone());
    let mut result = timezone_result(
        system.as_deref(),
        cli.as_deref(),
        exit["timezone"]["id"].as_str(),
    );
    result.detail = format!("{} · {}", super::field(exit, "ip"), result.detail);
    result.evidence["ip"] = exit["ip"].clone();
    result
}

pub(super) fn timezone_result(
    system: Option<&str>,
    cli: Option<&str>,
    exit: Option<&str>,
) -> CheckResult {
    let comparison = |local: Option<&str>| match (local, exit) {
        (Some(a), Some(b)) if a == b => tr("matching"),
        (Some(_), Some(_)) => tr("different_identifier"),
        _ => tr("unknown"),
    };
    let status = if system.is_none() || cli.is_none() || exit.is_none() {
        Status::Unknown
    } else if system == exit && cli == exit {
        Status::Passed
    } else {
        Status::Attention
    };
    CheckResult::new(
        status,
        message(
            "timezone_result",
            &[("cli", comparison(cli)), ("system", comparison(system))],
        ),
        message(
            "timezone_detail",
            &[
                ("system", system.unwrap_or("—").into()),
                ("cli", cli.unwrap_or("—").into()),
                ("exit", exit.unwrap_or("—").into()),
            ],
        ),
        json!({"system_timezone":system,"process_cli_timezone":cli,"exit_timezone":exit,"comparison":"IANA identifiers; aliases may have identical offsets"}),
    )
}

pub(super) fn claude() -> CheckResult {
    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let config = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|p| p.join(".claude")));
    let mut endpoint = env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut unreadable = false;
    if endpoint.is_none()
        && let Some(config) = &config
    {
        // Local settings take precedence over user settings. Never retain or export other fields.
        for name in ["settings.local.json", "settings.json"] {
            let path = config.join(name);
            if !path.exists() {
                continue;
            }
            let value = (|| {
                let file = fs::File::open(path).ok()?;
                let mut text = String::new();
                file.take(1024 * 1024 + 1).read_to_string(&mut text).ok()?;
                if text.len() > 1024 * 1024 {
                    return None;
                }
                serde_json::from_str::<Value>(&text).ok()
            })();
            if let Some(value) = value {
                endpoint = value
                    .pointer("/env/ANTHROPIC_BASE_URL")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned);
                if endpoint.is_some() {
                    break;
                }
            } else {
                unreadable = true;
            }
        }
    }
    if endpoint.is_none() && unreadable {
        return CheckResult::unavailable();
    }
    let installed = config.is_some_and(|path| path.is_dir())
        || home.is_some_and(|path| path.join(".claude.json").exists())
        || env::var_os("PATH").is_some_and(|path| {
            env::split_paths(&path).any(|dir| {
                if cfg!(windows) {
                    ["claude.exe", "claude.cmd", "claude.bat"]
                        .iter()
                        .any(|name| dir.join(name).is_file())
                } else {
                    dir.join("claude").is_file()
                }
            })
        });
    endpoint_result(endpoint.as_deref(), installed)
}

pub(super) fn endpoint_result(endpoint: Option<&str>, installed: bool) -> CheckResult {
    let Some(endpoint) = endpoint else {
        return if installed {
            CheckResult::new(
                Status::Info,
                "api.anthropic.com",
                tr("claude_default"),
                json!({"host":"api.anthropic.com","source":"default"}),
            )
        } else {
            CheckResult::new(
                Status::Unknown,
                tr("claude_missing"),
                tr("claude_scope"),
                Value::Null,
            )
        };
    };
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return CheckResult::unavailable();
    };
    let Some(host) = url.host_str() else {
        return CheckResult::unavailable();
    };
    if !matches!(url.scheme(), "https" | "http") {
        return CheckResult::unavailable();
    }
    let official = host.eq_ignore_ascii_case("api.anthropic.com")
        && url.scheme() == "https"
        && url.port_or_known_default() == Some(443);
    // URLs can contain API keys in userinfo, paths, fragments or query strings.
    // Only the host leaves this function; classification is not a connectivity test.
    CheckResult::new(
        if official {
            Status::Info
        } else {
            Status::Attention
        },
        host,
        tr(if official {
            "claude_official"
        } else {
            "claude_custom"
        }),
        json!({"host":host,"official":official}),
    )
}
