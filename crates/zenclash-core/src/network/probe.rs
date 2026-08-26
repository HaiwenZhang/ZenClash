use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const IP_RESPONSE_LIMIT: usize = 256 * 1024;
const LATENCY_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const MAX_LATENCY_TARGETS: usize = 16;

/// Built-in public-IP services compatible with Clash Party's network page.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PublicIpProvider {
    /// `api.ip.sb` `GeoIP` response.
    #[default]
    IpSb,
    /// `ipwho.is` `GeoIP` response.
    IpWhoIs,
    /// `api.ipapi.is` network intelligence response.
    IpApiIs,
}

impl PublicIpProvider {
    /// All providers in stable UI order.
    pub const ALL: [Self; 3] = [Self::IpSb, Self::IpWhoIs, Self::IpApiIs];

    /// Short provider label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::IpSb => "IP.SB",
            Self::IpWhoIs => "ipwho.is",
            Self::IpApiIs => "ipapi.is",
        }
    }

    const fn endpoint(self) -> &'static str {
        match self {
            Self::IpSb => "https://api.ip.sb/geoip",
            Self::IpWhoIs => "https://ipwho.is/",
            Self::IpApiIs => "https://api.ipapi.is/",
        }
    }
}

/// Route used for public network diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NetworkProbeRoute {
    /// Connect without an application-configured proxy.
    #[default]
    Direct,
    /// Send HTTP and HTTPS diagnostics through a local Mihomo HTTP endpoint.
    MihomoHttp {
        /// Listener host, normally loopback.
        host: String,
        /// HTTP or mixed listener port.
        port: u16,
    },
}

impl NetworkProbeRoute {
    /// Human-readable route shown beside diagnostic results.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Direct => "直连".into(),
            Self::MihomoHttp { host, port } => format!("Mihomo {host}:{port}"),
        }
    }
}

/// Normalized public address and location returned by a supported provider.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PublicIpInfo {
    /// Public IPv4 or IPv6 address.
    pub ip: String,
    /// Country display name.
    pub country: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: Option<String>,
    /// First-level administrative region.
    pub region: Option<String>,
    /// City name.
    pub city: Option<String>,
    /// Autonomous-system number.
    pub asn: Option<u64>,
    /// Autonomous-system organization.
    pub organization: Option<String>,
    /// Internet service provider.
    pub isp: Option<String>,
    /// Provider-reported proxy status.
    pub is_proxy: Option<bool>,
    /// Provider-reported VPN status.
    pub is_vpn: Option<bool>,
    /// IANA time-zone identifier.
    pub timezone: Option<String>,
    /// Approximate latitude.
    pub latitude: Option<f64>,
    /// Approximate longitude.
    pub longitude: Option<f64>,
}

/// Named HTTP(S) endpoint measured by the network diagnostics page.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkLatencyTarget {
    /// User-facing endpoint name.
    pub name: String,
    /// Absolute HTTP(S) URL.
    pub url: String,
}

impl NetworkLatencyTarget {
    /// Validates and normalizes a latency target.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized name, credentials, unsupported
    /// schemes, missing hosts, or malformed URLs.
    pub fn new(name: impl Into<String>, url: impl AsRef<str>) -> NetworkProbeResult<Self> {
        let name = name.into().trim().to_owned();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(NetworkProbeError::InvalidTarget(
                "探测名称必须为 1 到 64 个字符".into(),
            ));
        }
        let url = normalize_http_url(url.as_ref())?;
        Ok(Self {
            name,
            url: url.into(),
        })
    }
}

/// Default latency endpoints mirrored from Clash Party.
pub const DEFAULT_NETWORK_LATENCY_TARGETS: [(&str, &str); 3] = [
    ("Google", "https://www.google.com/generate_204"),
    ("Cloudflare", "https://www.cloudflare.com/cdn-cgi/trace"),
    ("GitHub", "https://github.com/"),
];

/// Result for one independent latency endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkLatencyResult {
    /// Target that was measured.
    pub target: NetworkLatencyTarget,
    /// End-to-end HTTP duration, absent after a failure.
    pub latency_ms: Option<u64>,
    /// Bounded error text for this target only.
    pub error: Option<String>,
}

/// Public-IP and latency results produced by one refresh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkProbeSnapshot {
    /// Route actually used for all requests.
    pub route: String,
    /// IP information when the selected provider succeeded.
    pub public_ip: Option<PublicIpInfo>,
    /// Provider failure without discarding latency results.
    pub public_ip_error: Option<String>,
    /// Independent latency endpoint results.
    pub latencies: Vec<NetworkLatencyResult>,
}

/// Errors produced while constructing or running network diagnostics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NetworkProbeError {
    /// Caller supplied an unsafe or malformed target.
    #[error("网络探测目标无效：{0}")]
    InvalidTarget(String),
    /// HTTP client construction or transport failed.
    #[error("网络探测请求失败：{0}")]
    Http(#[from] reqwest::Error),
    /// A response exceeded the defensive body limit.
    #[error("网络探测响应超过 {limit_kib} KiB 限制")]
    ResponseTooLarge {
        /// Maximum accepted response size in kibibytes.
        limit_kib: usize,
    },
    /// A provider returned successful HTTP with an incompatible document.
    #[error("公网 IP 服务响应无效：{0}")]
    InvalidResponse(String),
}

/// Result type for network diagnostics.
pub type NetworkProbeResult<T> = Result<T, NetworkProbeError>;

/// Cloneable HTTP diagnostics client with fixed route and bounded timeouts.
#[derive(Clone)]
pub struct NetworkProbeService {
    route: NetworkProbeRoute,
    http: reqwest::Client,
}

impl NetworkProbeService {
    /// Creates a diagnostics client for a direct or local-Mihomo route.
    ///
    /// # Errors
    ///
    /// Returns an error when the proxy route is invalid or the HTTP client
    /// cannot be constructed.
    pub fn new(route: NetworkProbeRoute) -> NetworkProbeResult<Self> {
        Self::with_timeouts(route, Duration::from_secs(5), Duration::from_secs(10))
    }

    fn with_timeouts(
        route: NetworkProbeRoute,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> NetworkProbeResult<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::limited(8));
        if let NetworkProbeRoute::MihomoHttp { host, port } = &route {
            if host.trim().is_empty() || *port == 0 {
                return Err(NetworkProbeError::InvalidTarget(
                    "Mihomo HTTP 代理地址或端口无效".into(),
                ));
            }
            let proxy_url = format!("http://{host}:{port}");
            builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
        }
        Ok(Self {
            route,
            http: builder.build()?,
        })
    }

    /// Fetches IP data and measures all latency targets concurrently.
    ///
    /// Provider and per-target failures are preserved in the returned
    /// snapshot, so one unavailable service does not erase other diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error before sending requests when there are too many or
    /// invalid targets.
    pub async fn snapshot(
        &self,
        provider: PublicIpProvider,
        targets: &[NetworkLatencyTarget],
    ) -> NetworkProbeResult<NetworkProbeSnapshot> {
        if targets.len() > MAX_LATENCY_TARGETS {
            return Err(NetworkProbeError::InvalidTarget(format!(
                "最多支持 {MAX_LATENCY_TARGETS} 个延迟目标"
            )));
        }
        let targets = targets
            .iter()
            .map(|target| NetworkLatencyTarget::new(&target.name, &target.url))
            .collect::<NetworkProbeResult<Vec<_>>>()?;
        let ip_request = self.fetch_public_ip(provider);
        let latency_requests = targets
            .into_iter()
            .map(|target| self.measure_target(target));
        let (public_ip, latencies) =
            tokio::join!(ip_request, futures_util::future::join_all(latency_requests));
        let (public_ip, public_ip_error) = match public_ip {
            Ok(info) => (Some(info), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Ok(NetworkProbeSnapshot {
            route: self.route.label(),
            public_ip,
            public_ip_error,
            latencies,
        })
    }

    /// Measures one validated HTTP(S) target through this service's route.
    ///
    /// Transport and HTTP failures are returned inside the result so callers
    /// can retain the target alongside its diagnostic error.
    pub async fn measure_target(&self, target: NetworkLatencyTarget) -> NetworkLatencyResult {
        match NetworkLatencyTarget::new(&target.name, &target.url) {
            Ok(target) => self.measure_latency(target).await,
            Err(error) => NetworkLatencyResult {
                target,
                latency_ms: None,
                error: Some(error.to_string()),
            },
        }
    }

    async fn fetch_public_ip(
        &self,
        provider: PublicIpProvider,
    ) -> NetworkProbeResult<PublicIpInfo> {
        self.fetch_public_ip_from(provider, provider.endpoint())
            .await
    }

    async fn fetch_public_ip_from(
        &self,
        provider: PublicIpProvider,
        endpoint: &str,
    ) -> NetworkProbeResult<PublicIpInfo> {
        let endpoint = normalize_http_url(endpoint)?;
        let response = self.http.get(endpoint).send().await?.error_for_status()?;
        let bytes = read_limited(response, IP_RESPONSE_LIMIT).await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| NetworkProbeError::InvalidResponse(error.to_string()))?;
        parse_public_ip(provider, &value)
    }

    async fn measure_latency(&self, target: NetworkLatencyTarget) -> NetworkLatencyResult {
        let started = Instant::now();
        let result = async {
            let response = self
                .http
                .get(&target.url)
                .send()
                .await?
                .error_for_status()?;
            consume_limited(response, LATENCY_RESPONSE_LIMIT).await
        }
        .await;
        match result {
            Ok(()) => NetworkLatencyResult {
                target,
                latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
                error: None,
            },
            Err(error) => NetworkLatencyResult {
                target,
                latency_ms: None,
                error: Some(error.to_string()),
            },
        }
    }
}

fn normalize_http_url(value: &str) -> NetworkProbeResult<reqwest::Url> {
    let trimmed = value.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&candidate)
        .map_err(|error| NetworkProbeError::InvalidTarget(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NetworkProbeError::InvalidTarget(
            "仅支持带主机名的 HTTP 或 HTTPS URL".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetworkProbeError::InvalidTarget(
            "探测 URL 不允许包含登录凭据".into(),
        ));
    }
    Ok(url)
}

async fn read_limited(response: reqwest::Response, limit: usize) -> NetworkProbeResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(NetworkProbeError::ResponseTooLarge {
            limit_kib: limit / 1024,
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(NetworkProbeError::ResponseTooLarge {
                limit_kib: limit / 1024,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn consume_limited(response: reqwest::Response, limit: usize) -> NetworkProbeResult<()> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(NetworkProbeError::ResponseTooLarge {
            limit_kib: limit / 1024,
        });
    }
    let mut received = 0_usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received = received.saturating_add(chunk.len());
        if received > limit {
            return Err(NetworkProbeError::ResponseTooLarge {
                limit_kib: limit / 1024,
            });
        }
    }
    Ok(())
}

fn parse_public_ip(provider: PublicIpProvider, value: &Value) -> NetworkProbeResult<PublicIpInfo> {
    let object = value
        .as_object()
        .ok_or_else(|| NetworkProbeError::InvalidResponse("顶层 JSON 不是对象".into()))?;
    let string = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| object.get(key).and_then(Value::as_f64);
    let boolean = |key: &str| object.get(key).and_then(Value::as_bool);
    let mut info = match provider {
        PublicIpProvider::IpSb => PublicIpInfo {
            ip: string("ip").unwrap_or_default(),
            country: string("country"),
            country_code: string("country_code"),
            city: string("city"),
            asn: parse_asn(object.get("asn")),
            organization: string("asn_organization"),
            latitude: number("latitude"),
            longitude: number("longitude"),
            ..Default::default()
        },
        PublicIpProvider::IpWhoIs => {
            let connection = object.get("connection").and_then(Value::as_object);
            let timezone = object.get("timezone").and_then(Value::as_object);
            PublicIpInfo {
                ip: string("ip").unwrap_or_default(),
                country: string("country"),
                country_code: string("country_code"),
                region: string("region"),
                city: string("city"),
                asn: connection.and_then(|item| parse_asn(item.get("asn"))),
                organization: nested_string(connection, "org"),
                isp: nested_string(connection, "isp"),
                timezone: nested_string(timezone, "id"),
                latitude: number("latitude"),
                longitude: number("longitude"),
                ..Default::default()
            }
        }
        PublicIpProvider::IpApiIs => {
            let location = object.get("location").and_then(Value::as_object);
            let asn = object.get("asn").and_then(Value::as_object);
            PublicIpInfo {
                ip: string("ip").unwrap_or_default(),
                country: nested_string(location, "country"),
                country_code: nested_string(location, "country_code"),
                region: nested_string(location, "state"),
                city: nested_string(location, "city"),
                asn: asn.and_then(|item| parse_asn(item.get("asn"))),
                organization: nested_string(asn, "org"),
                is_proxy: boolean("is_proxy"),
                is_vpn: boolean("is_vpn"),
                timezone: nested_string(location, "timezone"),
                latitude: nested_number(location, "latitude"),
                longitude: nested_number(location, "longitude"),
                ..Default::default()
            }
        }
    };
    info.ip = info.ip.trim().to_owned();
    if info.ip.is_empty() {
        return Err(NetworkProbeError::InvalidResponse(
            "响应缺少公网 IP 字段".into(),
        ));
    }
    Ok(info)
}

fn parse_asn(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value.as_u64().or_else(|| {
            value
                .as_str()?
                .trim()
                .trim_start_matches(|character: char| character.eq_ignore_ascii_case(&'a'))
                .trim_start_matches(|character: char| character.eq_ignore_ascii_case(&'s'))
                .parse()
                .ok()
        })
    })
}

fn nested_string(object: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    object?.get(key)?.as_str().map(str::to_owned)
}

fn nested_number(object: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<f64> {
    object?.get(key)?.as_f64()
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    async fn serve_once(status: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}/probe")
    }

    #[test]
    fn normalizes_targets_and_rejects_credentials() {
        let target = NetworkLatencyTarget::new(" Example ", "example.com/health").unwrap();
        assert_eq!(target.name, "Example");
        assert_eq!(target.url, "https://example.com/health");
        assert!(NetworkLatencyTarget::new("private", "https://user:pass@example.com").is_err());
    }

    #[test]
    fn parses_each_supported_provider_shape() {
        let sb = serde_json::json!({
            "ip": "203.0.113.4", "country": "Example", "country_code": "EX",
            "asn": "AS64500", "asn_organization": "Example Net"
        });
        let who = serde_json::json!({
            "ip": "2001:db8::1", "region": "North", "connection": {
                "asn": 64501, "org": "Example Org", "isp": "Example ISP"
            }, "timezone": {"id": "Etc/UTC"}
        });
        let api = serde_json::json!({
            "ip": "198.51.100.8", "is_proxy": true, "is_vpn": false,
            "location": {"country": "Example", "latitude": 1.5},
            "asn": {"asn": 64502, "org": "Third Org"}
        });

        assert_eq!(
            parse_public_ip(PublicIpProvider::IpSb, &sb).unwrap().asn,
            Some(64500)
        );
        assert_eq!(
            parse_public_ip(PublicIpProvider::IpWhoIs, &who)
                .unwrap()
                .isp
                .as_deref(),
            Some("Example ISP")
        );
        assert_eq!(
            parse_public_ip(PublicIpProvider::IpApiIs, &api)
                .unwrap()
                .is_proxy,
            Some(true)
        );
    }

    #[tokio::test]
    async fn uses_real_http_for_ip_and_latency() {
        let ip_url = serve_once(
            "200 OK",
            r#"{"ip":"203.0.113.9","country":"Example","asn":64500}"#,
        )
        .await;
        let latency_url = serve_once("204 No Content", "").await;
        let service = NetworkProbeService::with_timeouts(
            NetworkProbeRoute::Direct,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .unwrap();

        let info = service
            .fetch_public_ip_from(PublicIpProvider::IpSb, &ip_url)
            .await
            .unwrap();
        let latency = service
            .measure_latency(NetworkLatencyTarget::new("local", latency_url).unwrap())
            .await;

        assert_eq!(info.ip, "203.0.113.9");
        assert!(latency.error.is_none());
        assert!(latency.latency_ms.is_some());
    }

    #[tokio::test]
    async fn reports_oversized_ip_response() {
        let body = "x".repeat(IP_RESPONSE_LIMIT + 1);
        let url = serve_once("200 OK", &body).await;
        let service = NetworkProbeService::new(NetworkProbeRoute::Direct).unwrap();

        let error = service
            .fetch_public_ip_from(PublicIpProvider::IpSb, &url)
            .await
            .unwrap_err();

        assert!(matches!(error, NetworkProbeError::ResponseTooLarge { .. }));
    }
}
