//! Read-only AI network checks. Observations describe this process and request route,
//! not the network stack of another application or account safety.

mod local;
#[cfg(test)]
mod tests;

use crate::NetworkProbeRoute;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{net::IpAddr, time::Duration};

/// Stable identities for the seven independently runnable checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Public exit address and location.
    ExitIp,
    /// Process environment, OS proxy and Mihomo TUN configuration.
    Proxy,
    /// Host IPv6 reachability.
    Ipv6,
    /// Configured system DNS servers.
    Dns,
    /// Third-party reputation and abuse observations.
    Risk,
    /// System and process time zone identifiers versus exit location.
    Timezone,
    /// Locally configured Claude Code endpoint.
    Claude,
}

impl Check {
    /// Stable display and execution order.
    pub const ALL: [Self; 7] = [
        Self::ExitIp,
        Self::Proxy,
        Self::Ipv6,
        Self::Dns,
        Self::Risk,
        Self::Timezone,
        Self::Claude,
    ];

    /// Locale suffix and stable UI identifier.
    pub const fn key(self) -> &'static str {
        match self {
            Self::ExitIp => "exit_ip",
            Self::Proxy => "proxy",
            Self::Ipv6 => "ipv6",
            Self::Dns => "dns",
            Self::Risk => "risk",
            Self::Timezone => "timezone",
            Self::Claude => "claude",
        }
    }
}

/// Unknown and failed observations must never count as passing checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// An observation without a safety verdict.
    Info,
    /// Required checks completed without a flagged signal.
    Passed,
    /// A configuration or reputation signal needs review.
    Attention,
    /// A high-risk reputation signal was returned.
    Failed,
    /// Insufficient evidence to make a determination.
    Unknown,
}

/// A bounded, credential-free report row suitable for display and clipboard export.
#[derive(Clone, Debug)]
pub struct CheckResult {
    status: Status,
    summary: String,
    detail: String,
    evidence: Value,
}

impl CheckResult {
    /// Observation classification.
    pub fn status(&self) -> Status {
        self.status
    }
    /// Primary localized result text.
    pub fn summary(&self) -> &str {
        &self.summary
    }
    /// Scope, source or recovery guidance.
    pub fn detail(&self) -> &str {
        &self.detail
    }
    /// Selected diagnostic fields, excluding credentials and full configuration.
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }

    fn new(
        status: Status,
        summary: impl Into<String>,
        detail: impl Into<String>,
        evidence: Value,
    ) -> Self {
        Self {
            status,
            summary: summary.into(),
            detail: detail.into(),
            evidence,
        }
    }

    /// An unavailable source or invalid response, never a passing result.
    pub fn unavailable() -> Self {
        Self::new(
            Status::Unknown,
            tr("unavailable"),
            tr("retry_hint"),
            Value::Null,
        )
    }
}

fn tr(key: &str) -> String {
    zenclash_i18n::text(&format!("ai_network.{key}"))
}
fn message(key: &str, values: &[(&str, String)]) -> String {
    zenclash_i18n::text_with(&format!("ai_network.{key}"), values)
}

/// A test run owns its client and cached exit observation. A new run rechecks the exit.
pub struct Diagnostics {
    http: Result<reqwest::Client, reqwest::Error>,
    exit: Option<Result<Value, ()>>,
    tun: Option<bool>,
}

impl Diagnostics {
    /// Creates a run using an explicit request route and optional TUN configuration.
    pub fn new(route: &NetworkProbeRoute, tun: Option<bool>) -> Self {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")));
        if let NetworkProbeRoute::MihomoHttp { host, port } = route {
            match reqwest::Proxy::all(format!("http://{host}:{port}")) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => {
                    return Self {
                        http: Err(error),
                        exit: None,
                        tun,
                    };
                }
            }
        }
        Self {
            http: builder.build(),
            exit: None,
            tun,
        }
    }

    /// Runs one bounded check without changing host configuration.
    pub async fn check(&mut self, check: Check) -> CheckResult {
        match check {
            Check::Proxy | Check::Dns | Check::Claude => {
                let tun = self.tun;
                tokio::task::spawn_blocking(move || match check {
                    Check::Proxy => local::proxy(tun),
                    Check::Dns => local::dns(),
                    _ => local::claude(),
                })
                .await
                .unwrap_or_else(|_| CheckResult::unavailable())
            }
            Check::Ipv6 => self.ipv6().await,
            Check::ExitIp | Check::Risk | Check::Timezone => {
                let Ok(exit) = self.exit().await else {
                    return CheckResult::unavailable();
                };
                match check {
                    Check::ExitIp => CheckResult::new(
                        Status::Info,
                        format!(
                            "{} · {} / {}",
                            field(&exit, "ip"),
                            field(&exit, "country"),
                            field(&exit, "city")
                        ),
                        format!(
                            "ipwho.is · {}",
                            exit.pointer("/connection/isp")
                                .and_then(Value::as_str)
                                .unwrap_or("—")
                        ),
                        json!({"ip":exit["ip"], "country":exit["country"], "city":exit["city"], "timezone":exit["timezone"]["id"]}),
                    ),
                    Check::Risk => self.risk(&exit).await,
                    _ => tokio::task::spawn_blocking(move || local::timezone(&exit))
                        .await
                        .unwrap_or_else(|_| CheckResult::unavailable()),
                }
            }
        }
    }

    async fn exit(&mut self) -> Result<Value, ()> {
        if self.exit.is_none() {
            let result = self.json("https://ipwho.is/", &[]).await.and_then(|value| {
                if value["success"].as_bool() == Some(true)
                    && field(&value, "ip").parse::<IpAddr>().is_ok()
                {
                    Ok(value)
                } else {
                    Err(())
                }
            });
            self.exit = Some(result);
        }
        self.exit.as_ref().cloned().unwrap_or(Err(()))
    }

    async fn json(&self, url: &str, query: &[(&str, &str)]) -> Result<Value, ()> {
        let response = self
            .http
            .as_ref()
            .map_err(|_| ())?
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|_| ())?
            .error_for_status()
            .map_err(|_| ())?;
        let body = read_body(response).await?;
        serde_json::from_slice(&body).map_err(|_| ())
    }

    async fn ipv6(&self) -> CheckResult {
        // A separate IPv6-only, no-application-proxy probe observes the host route.
        // An OS TUN may still intercept it; a failure does not prove IPv6 is disabled.
        let result = async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(8))
                .build()
                .map_err(|_| ())?;
            let response = client
                .get("https://api6.ipify.org?format=json")
                .send()
                .await
                .map_err(|_| ())?
                .error_for_status()
                .map_err(|_| ())?;
            let value: Value =
                serde_json::from_slice(&read_body(response).await?).map_err(|_| ())?;
            let ip = field(&value, "ip")
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| ())?;
            Ok::<_, ()>(ip)
        }
        .await;
        match result {
            Ok(ip) => CheckResult::new(
                Status::Attention,
                ip.to_string(),
                tr("ipv6_found"),
                json!({"host_ipv6":ip.to_string(), "source":"api6.ipify.org"}),
            ),
            Err(()) => CheckResult::new(
                Status::Unknown,
                tr("ipv6_missing"),
                tr("ipv6_unknown"),
                Value::Null,
            ),
        }
    }

    async fn risk(&self, exit: &Value) -> CheckResult {
        let ip = field(exit, "ip");
        let risk_url = format!("https://proxycheck.io/v2/{ip}");
        let spam_query = [("json", "1"), ("ip", ip)];
        let (risk, spam) = tokio::join!(
            self.json(&risk_url, &[("risk", "1"), ("vpn", "1"), ("asn", "1")]),
            self.json("https://api.stopforumspam.org/api", &spam_query)
        );
        risk_result(ip, risk.ok().as_ref(), spam.ok().as_ref())
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("—")
}

async fn read_body(response: reqwest::Response) -> Result<Vec<u8>, ()> {
    const LIMIT: usize = 256 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > LIMIT as u64)
    {
        return Err(());
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        if body.len().saturating_add(chunk.len()) > LIMIT {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn risk_result(ip: &str, risk: Option<&Value>, spam: Option<&Value>) -> CheckResult {
    let score = risk
        .filter(|v| v["status"] == "ok")
        .and_then(|v| v[ip]["risk"].as_u64())
        .filter(|v| *v <= 100);
    let appears = spam
        .filter(|v| v["success"].as_bool() == Some(true) || v["success"].as_u64() == Some(1))
        .and_then(|v| {
            v["ip"]["appears"]
                .as_bool()
                .or_else(|| v["ip"]["appears"].as_u64().map(|n| n != 0))
        });
    let status = match (score, appears) {
        (Some(70..), _) => Status::Failed,
        (Some(30..), _) | (_, Some(true)) => Status::Attention,
        (Some(_), Some(false)) => Status::Passed,
        _ => Status::Unknown,
    };
    CheckResult::new(
        status,
        message(
            "risk_result",
            &[
                ("score", score.map_or_else(|| "—".into(), |v| v.to_string())),
                (
                    "spam",
                    tr(match appears {
                        Some(true) => "spam_found",
                        Some(false) => "spam_clear",
                        None => "spam_unknown",
                    }),
                ),
            ],
        ),
        format!("{ip} · {}", tr("risk_source")),
        json!({"ip":ip, "risk_score":score, "abuse_listed":appears,
            "type":risk.and_then(|v| v[ip]["type"].as_str())}),
    )
}
