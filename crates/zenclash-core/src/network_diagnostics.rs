//! Independent, route-aware network diagnostics and support-safe export.

use std::{future::Future, pin::Pin, sync::Arc, time::Instant};

use futures_util::future::join_all;
use serde::Serialize;
use thiserror::Error;

use crate::{
    CaptureStatus, DnsQueryResponse, DnsRecordType, MihomoClient, NetworkLatencyTarget,
    NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot, OperationalStatus,
    ProviderCatalog, PublicIpProvider, VersionInfo,
};

/// Route or subsystem responsible for one diagnostic step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticRoute {
    /// Mihomo's authenticated external controller.
    Controller,
    /// Read-only state owned by the local application.
    Local,
    /// HTTP requests that explicitly bypass Mihomo.
    Direct,
    /// HTTP requests that explicitly traverse Mihomo.
    Mihomo,
}

/// Stable identity for one independently executed diagnostic step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticStepKind {
    /// Controller authentication and version response.
    Controller,
    /// System Proxy and TUN observation.
    Capture,
    /// IPv4 DNS resolution through Mihomo.
    DnsA,
    /// IPv6 DNS resolution through Mihomo.
    DnsAaaa,
    /// Public-IP and latency requests that bypass Mihomo.
    NetworkDirect,
    /// Public-IP and latency requests through Mihomo.
    NetworkMihomo,
    /// Proxy-provider catalog readback.
    ProxyProviders,
    /// Rule-provider catalog readback.
    RuleProviders,
}

impl DiagnosticStepKind {
    const fn route(self) -> DiagnosticRoute {
        match self {
            Self::Controller
            | Self::DnsA
            | Self::DnsAaaa
            | Self::ProxyProviders
            | Self::RuleProviders => DiagnosticRoute::Controller,
            Self::Capture => DiagnosticRoute::Local,
            Self::NetworkDirect => DiagnosticRoute::Direct,
            Self::NetworkMihomo => DiagnosticRoute::Mihomo,
        }
    }
}

/// Typed successful value produced by a diagnostic step.
#[derive(Clone, Debug, PartialEq)]
pub enum DiagnosticData {
    /// Authenticated controller version.
    Controller(VersionInfo),
    /// Current local capture facts.
    Capture(CaptureStatus),
    /// One DNS response, including status, answers and TTL values.
    Dns(DnsQueryResponse),
    /// One public network snapshot on an explicit route.
    Network(NetworkProbeSnapshot),
    /// Proxy or rule provider catalog.
    Providers(ProviderCatalog),
}

/// Failure retained for one diagnostic step without aborting the report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticFailure {
    /// Human-readable failure for the interactive diagnostics view.
    pub message: String,
}

/// Timed outcome of one independently executed diagnostic step.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticStep {
    /// Stable step identity.
    pub kind: DiagnosticStepKind,
    /// Route or subsystem actually used.
    pub route: DiagnosticRoute,
    /// Unix timestamp in milliseconds when the step completed.
    pub completed_at_ms: u64,
    /// Wall-clock execution duration in milliseconds.
    pub duration_ms: u64,
    /// Typed result retained independently from all other steps.
    pub outcome: Result<DiagnosticData, DiagnosticFailure>,
}

/// Complete result of one diagnostics run.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticReport {
    /// Unix timestamp in milliseconds when the run started.
    pub started_at_ms: u64,
    /// Independently executed steps in stable display order.
    pub steps: Vec<DiagnosticStep>,
}

impl DiagnosticReport {
    /// Returns a step by its stable identity.
    #[must_use]
    pub fn step(&self, kind: DiagnosticStepKind) -> Option<&DiagnosticStep> {
        self.steps.iter().find(|step| step.kind == kind)
    }
}

/// Validated inputs for one diagnostics run.
#[derive(Clone, Debug)]
pub struct DiagnosticPlan {
    dns_name: String,
    provider: PublicIpProvider,
    latency_targets: Vec<NetworkLatencyTarget>,
    mihomo_route: Option<NetworkProbeRoute>,
}

impl DiagnosticPlan {
    /// Creates a plan with an explicit DNS name and latency targets.
    ///
    /// The Mihomo network step remains present but reports unavailable until
    /// [`Self::with_mihomo_route`] supplies a usable HTTP or mixed listener.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, whitespace-containing or control-character
    /// DNS names.
    pub fn new(
        dns_name: impl Into<String>,
        provider: PublicIpProvider,
        latency_targets: Vec<NetworkLatencyTarget>,
    ) -> NetworkDiagnosticsResult<Self> {
        let dns_name = dns_name.into().trim().trim_end_matches('.').to_owned();
        if dns_name.is_empty()
            || dns_name.len() > 253
            || dns_name.chars().any(char::is_whitespace)
            || dns_name.chars().any(char::is_control)
        {
            return Err(NetworkDiagnosticsError::InvalidPlan(
                "DNS 查询名称无效".into(),
            ));
        }
        Ok(Self {
            dns_name,
            provider,
            latency_targets,
            mihomo_route: None,
        })
    }

    /// Adds the local Mihomo HTTP route used by the explicit comparison step.
    ///
    /// # Errors
    ///
    /// Rejects a direct route, blank host or zero port.
    pub fn with_mihomo_route(mut self, route: NetworkProbeRoute) -> NetworkDiagnosticsResult<Self> {
        match &route {
            NetworkProbeRoute::MihomoHttp { host, port }
                if !host.trim().is_empty() && *port != 0 =>
            {
                self.mihomo_route = Some(route);
                Ok(self)
            }
            NetworkProbeRoute::Direct | NetworkProbeRoute::MihomoHttp { .. } => Err(
                NetworkDiagnosticsError::InvalidPlan("Mihomo 诊断路由无效".into()),
            ),
        }
    }
}

/// Errors that prevent a diagnostics run from being constructed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NetworkDiagnosticsError {
    /// The caller supplied an invalid diagnostic plan.
    #[error("网络诊断计划无效：{0}")]
    InvalidPlan(String),
}

/// Result type for constructing network diagnostics.
pub type NetworkDiagnosticsResult<T> = Result<T, NetworkDiagnosticsError>;

/// Marker requiring the default redacted support export policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct SupportSafe;

/// A serialized diagnostic bundle safe to copy into a support request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportBundle {
    /// Unix timestamp in milliseconds when the bundle was generated.
    pub generated_at_ms: u64,
    /// Pretty-printed JSON with secrets, URLs, addresses and user paths omitted.
    pub json: String,
}

/// Runs independent controller, capture, DNS, provider, and route diagnostics.
#[derive(Clone)]
pub struct NetworkDiagnostics {
    backend: Arc<dyn DiagnosticBackend>,
}

impl NetworkDiagnostics {
    /// Creates diagnostics backed by the live Mihomo client and operational state.
    #[must_use]
    pub fn new(client: MihomoClient, operational_status: Arc<OperationalStatus>) -> Self {
        Self {
            backend: Arc::new(LiveDiagnosticBackend {
                client,
                operational_status,
            }),
        }
    }

    /// Runs every planned step independently and preserves partial success.
    #[must_use]
    pub async fn run(&self, plan: DiagnosticPlan) -> DiagnosticReport {
        let started_at_ms = now_ms();
        let requests = vec![
            (
                DiagnosticStepKind::Controller,
                DiagnosticRequest::Controller,
            ),
            (DiagnosticStepKind::Capture, DiagnosticRequest::Capture),
            (
                DiagnosticStepKind::DnsA,
                DiagnosticRequest::Dns {
                    name: plan.dns_name.clone(),
                    record_type: DnsRecordType::A,
                },
            ),
            (
                DiagnosticStepKind::DnsAaaa,
                DiagnosticRequest::Dns {
                    name: plan.dns_name,
                    record_type: DnsRecordType::Aaaa,
                },
            ),
            (
                DiagnosticStepKind::NetworkDirect,
                DiagnosticRequest::Network {
                    route: Some(NetworkProbeRoute::Direct),
                    provider: plan.provider,
                    targets: plan.latency_targets.clone(),
                },
            ),
            (
                DiagnosticStepKind::NetworkMihomo,
                DiagnosticRequest::Network {
                    route: plan.mihomo_route,
                    provider: plan.provider,
                    targets: plan.latency_targets,
                },
            ),
            (
                DiagnosticStepKind::ProxyProviders,
                DiagnosticRequest::ProxyProviders,
            ),
            (
                DiagnosticStepKind::RuleProviders,
                DiagnosticRequest::RuleProviders,
            ),
        ];
        let steps = join_all(
            requests
                .into_iter()
                .map(|(kind, request)| execute_step(self.backend.clone(), kind, request)),
        )
        .await;
        DiagnosticReport {
            started_at_ms,
            steps,
        }
    }

    /// Projects a report into a strictly allow-listed support-safe JSON bundle.
    ///
    /// Controller addresses and secrets, DNS names and answers, public IPs,
    /// target URLs, provider names and raw error messages are intentionally not
    /// present in this representation.
    #[must_use]
    pub fn export(&self, report: &DiagnosticReport, _policy: SupportSafe) -> SupportBundle {
        let generated_at_ms = now_ms();
        let safe = SafeReport {
            schema: 1,
            generated_at_ms,
            started_at_ms: report.started_at_ms,
            steps: report.steps.iter().map(SafeStep::from).collect(),
        };
        let json = serde_json::to_string_pretty(&safe).unwrap_or_else(|error| {
            tracing::error!(%error, "failed to serialize support-safe diagnostics");
            r#"{"schema":1,"generated_at_ms":0,"started_at_ms":0,"steps":[]}"#.into()
        });
        SupportBundle {
            generated_at_ms,
            json,
        }
    }

    #[cfg(test)]
    fn from_backend(backend: Arc<dyn DiagnosticBackend>) -> Self {
        Self { backend }
    }
}

async fn execute_step(
    backend: Arc<dyn DiagnosticBackend>,
    kind: DiagnosticStepKind,
    request: DiagnosticRequest,
) -> DiagnosticStep {
    let started = Instant::now();
    let outcome = backend
        .execute(request)
        .await
        .map_err(|message| DiagnosticFailure { message });
    DiagnosticStep {
        kind,
        route: kind.route(),
        completed_at_ms: now_ms(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        outcome,
    }
}

#[derive(Clone)]
enum DiagnosticRequest {
    Controller,
    Capture,
    Dns {
        name: String,
        record_type: DnsRecordType,
    },
    Network {
        route: Option<NetworkProbeRoute>,
        provider: PublicIpProvider,
        targets: Vec<NetworkLatencyTarget>,
    },
    ProxyProviders,
    RuleProviders,
}

type DiagnosticFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DiagnosticData, String>> + Send + 'a>>;

trait DiagnosticBackend: Send + Sync {
    fn execute(&self, request: DiagnosticRequest) -> DiagnosticFuture<'_>;
}

struct LiveDiagnosticBackend {
    client: MihomoClient,
    operational_status: Arc<OperationalStatus>,
}

impl DiagnosticBackend for LiveDiagnosticBackend {
    fn execute(&self, request: DiagnosticRequest) -> DiagnosticFuture<'_> {
        let client = self.client.clone();
        let operational_status = self.operational_status.clone();
        Box::pin(async move {
            match request {
                DiagnosticRequest::Controller => client
                    .version()
                    .await
                    .map(DiagnosticData::Controller)
                    .map_err(|error| error.to_string()),
                DiagnosticRequest::Capture => Ok(DiagnosticData::Capture(
                    operational_status.snapshot().capture,
                )),
                DiagnosticRequest::Dns { name, record_type } => client
                    .dns_query(&name, record_type)
                    .await
                    .map(DiagnosticData::Dns)
                    .map_err(|error| error.to_string()),
                DiagnosticRequest::Network {
                    route,
                    provider,
                    targets,
                } => {
                    let route = route.ok_or_else(|| {
                        "Mihomo 没有可用的 HTTP 或 mixed 入站，无法执行显式路径探测".to_owned()
                    })?;
                    let service =
                        NetworkProbeService::new(route).map_err(|error| error.to_string())?;
                    service
                        .snapshot(provider, &targets)
                        .await
                        .map(DiagnosticData::Network)
                        .map_err(|error| error.to_string())
                }
                DiagnosticRequest::ProxyProviders => client
                    .proxy_provider_catalog()
                    .await
                    .map(DiagnosticData::Providers)
                    .map_err(|error| error.to_string()),
                DiagnosticRequest::RuleProviders => client
                    .rule_provider_catalog()
                    .await
                    .map(DiagnosticData::Providers)
                    .map_err(|error| error.to_string()),
            }
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SafeReport {
    schema: u8,
    generated_at_ms: u64,
    started_at_ms: u64,
    steps: Vec<SafeStep>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SafeStep {
    kind: DiagnosticStepKind,
    route: DiagnosticRoute,
    completed_at_ms: u64,
    duration_ms: u64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    facts: Option<SafeFacts>,
}

impl From<&DiagnosticStep> for SafeStep {
    fn from(step: &DiagnosticStep) -> Self {
        let (status, facts) = match &step.outcome {
            Ok(data) => ("success", Some(SafeFacts::from(data))),
            Err(_) => ("failed", None),
        };
        Self {
            kind: step.kind,
            route: step.route,
            completed_at_ms: step.completed_at_ms,
            duration_ms: step.duration_ms,
            status,
            facts,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum SafeFacts {
    Controller {
        meta: bool,
    },
    Capture {
        system_proxy_observed: bool,
        tun_observed: bool,
    },
    Dns {
        response_status: u16,
        answer_count: usize,
        ttl_seconds: Vec<u32>,
    },
    Network {
        public_ip_available: bool,
        public_ip_failed: bool,
        successful_targets: usize,
        failed_targets: usize,
        latencies_ms: Vec<u64>,
    },
    Providers {
        provider_count: usize,
        item_count: usize,
    },
}

impl From<&DiagnosticData> for SafeFacts {
    fn from(data: &DiagnosticData) -> Self {
        match data {
            DiagnosticData::Controller(version) => Self::Controller { meta: version.meta },
            DiagnosticData::Capture(capture) => Self::Capture {
                system_proxy_observed: capture.system_proxy.value().is_some(),
                tun_observed: capture.tun.value().is_some(),
            },
            DiagnosticData::Dns(response) => Self::Dns {
                response_status: response.status,
                answer_count: response.answer.len(),
                ttl_seconds: response.answer.iter().map(|answer| answer.ttl).collect(),
            },
            DiagnosticData::Network(snapshot) => {
                let latencies_ms = snapshot
                    .latencies
                    .iter()
                    .filter_map(|result| result.latency_ms)
                    .collect::<Vec<_>>();
                Self::Network {
                    public_ip_available: snapshot.public_ip.is_some(),
                    public_ip_failed: snapshot.public_ip_error.is_some(),
                    successful_targets: latencies_ms.len(),
                    failed_targets: snapshot.latencies.len().saturating_sub(latencies_ms.len()),
                    latencies_ms,
                }
            }
            DiagnosticData::Providers(catalog) => Self::Providers {
                provider_count: catalog.providers.len(),
                item_count: catalog
                    .providers
                    .values()
                    .map(|provider| provider.proxies.len().saturating_add(provider.rule_count))
                    .sum(),
            },
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{DnsAnswer, NetworkLatencyResult, Provider};

    struct FakeBackend;

    impl DiagnosticBackend for FakeBackend {
        fn execute(&self, request: DiagnosticRequest) -> DiagnosticFuture<'_> {
            Box::pin(async move {
                match request {
                    DiagnosticRequest::Controller => Ok(DiagnosticData::Controller(VersionInfo {
                        meta: true,
                        version: "v-test".into(),
                    })),
                    DiagnosticRequest::Capture => {
                        Ok(DiagnosticData::Capture(CaptureStatus::default()))
                    }
                    DiagnosticRequest::Dns {
                        record_type: DnsRecordType::A,
                        ..
                    } => Err("A resolver unavailable".into()),
                    DiagnosticRequest::Dns {
                        record_type: DnsRecordType::Aaaa,
                        ..
                    } => Ok(DiagnosticData::Dns(DnsQueryResponse {
                        answer: vec![DnsAnswer {
                            ttl: 60,
                            data: "2001:db8::1".into(),
                            ..DnsAnswer::default()
                        }],
                        ..DnsQueryResponse::default()
                    })),
                    DiagnosticRequest::Network { route: None, .. } => {
                        Err("Mihomo route unavailable".into())
                    }
                    DiagnosticRequest::Network {
                        route: Some(route), ..
                    } => Ok(DiagnosticData::Network(NetworkProbeSnapshot {
                        route: route.label(),
                        latencies: vec![NetworkLatencyResult {
                            target: NetworkLatencyTarget::new(
                                "probe",
                                "https://example.com/secret?token=raw",
                            )
                            .unwrap(),
                            latency_ms: Some(25),
                            error: None,
                        }],
                        ..NetworkProbeSnapshot::default()
                    })),
                    DiagnosticRequest::ProxyProviders | DiagnosticRequest::RuleProviders => {
                        Ok(DiagnosticData::Providers(ProviderCatalog {
                            providers: BTreeMap::from([(
                                "private-provider".into(),
                                Provider {
                                    name: "private-provider".into(),
                                    test_url: "https://token@example.com/check?token=raw".into(),
                                    rule_count: 2,
                                    ..Provider::default()
                                },
                            )]),
                        }))
                    }
                }
            })
        }
    }

    fn test_plan() -> DiagnosticPlan {
        DiagnosticPlan::new(
            "secret.example",
            PublicIpProvider::IpSb,
            vec![
                NetworkLatencyTarget::new(
                    "target-with-secret",
                    "https://example.com/secret?token=raw",
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn every_step_retains_independent_status_time_and_route() {
        let diagnostics = NetworkDiagnostics::from_backend(Arc::new(FakeBackend));

        let report = diagnostics.run(test_plan()).await;

        assert_eq!(report.steps.len(), 8);
        assert!(
            report
                .step(DiagnosticStepKind::DnsA)
                .unwrap()
                .outcome
                .is_err()
        );
        assert!(
            report
                .step(DiagnosticStepKind::DnsAaaa)
                .unwrap()
                .outcome
                .is_ok()
        );
        assert_eq!(
            report
                .step(DiagnosticStepKind::NetworkDirect)
                .unwrap()
                .route,
            DiagnosticRoute::Direct
        );
        assert_eq!(
            report
                .step(DiagnosticStepKind::NetworkMihomo)
                .unwrap()
                .route,
            DiagnosticRoute::Mihomo
        );
        assert!(
            report
                .steps
                .iter()
                .all(|step| step.completed_at_ms >= report.started_at_ms)
        );
    }

    #[tokio::test]
    async fn support_bundle_uses_only_allow_listed_facts() {
        let diagnostics = NetworkDiagnostics::from_backend(Arc::new(FakeBackend));
        let mut report = diagnostics.run(test_plan()).await;
        report
            .step(DiagnosticStepKind::DnsA)
            .expect("DNS step")
            .outcome
            .as_ref()
            .unwrap_err();
        report.steps[2].outcome = Err(DiagnosticFailure {
            message: "Bearer controller-secret /Users/alice/config.yaml https://subscription.example/path?token=raw".into(),
        });

        let bundle = diagnostics.export(&report, SupportSafe);

        for secret in [
            "controller-secret",
            "/Users/alice",
            "subscription.example",
            "token=raw",
            "secret.example",
            "2001:db8::1",
            "private-provider",
            "target-with-secret",
        ] {
            assert!(!bundle.json.contains(secret), "leaked {secret}");
        }
        assert!(bundle.json.contains("network-direct"));
        assert!(bundle.json.contains("\"status\": \"failed\""));
    }
}
