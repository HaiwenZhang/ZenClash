//! Independent step execution and live controller/network observations.

use std::{future::Future, pin::Pin, sync::Arc, time::Instant};

use futures_util::future::join_all;

use super::{
    DiagnosticData, DiagnosticFailure, DiagnosticPlan, DiagnosticReport, DiagnosticStep,
    DiagnosticStepKind, SupportBundle, SupportSafe, unix_time_ms,
};
use crate::{
    DnsRecordType, MihomoClient, NetworkLatencyTarget, NetworkProbeRoute, NetworkProbeService,
    OperationalStatus, PublicIpProvider,
};

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
        let started_at_ms = unix_time_ms();
        let requests = vec![
            DiagnosticRequest::Controller,
            DiagnosticRequest::Capture,
            DiagnosticRequest::Dns {
                name: plan.dns_name.clone(),
                record_type: DnsRecordType::A,
            },
            DiagnosticRequest::Dns {
                name: plan.dns_name,
                record_type: DnsRecordType::Aaaa,
            },
            DiagnosticRequest::Network {
                route: Some(NetworkProbeRoute::Direct),
                provider: plan.provider,
                targets: plan.latency_targets.clone(),
            },
            DiagnosticRequest::Network {
                route: plan.mihomo_route,
                provider: plan.provider,
                targets: plan.latency_targets,
            },
            DiagnosticRequest::ProxyProviders,
            DiagnosticRequest::RuleProviders,
        ];
        let steps = join_all(
            requests
                .into_iter()
                .map(|request| execute_step(self.backend.clone(), request)),
        )
        .await;
        DiagnosticReport {
            started_at_ms,
            steps,
        }
    }

    /// Exports a report using the same support-safe policy as
    /// [`DiagnosticReport::export_support_bundle`].
    ///
    /// This compatibility entry point does not use the execution backend.
    /// New callers can export directly from the completed report.
    #[must_use]
    pub fn export(&self, report: &DiagnosticReport, policy: SupportSafe) -> SupportBundle {
        report.export_support_bundle(policy)
    }

    #[cfg(test)]
    pub(super) fn from_backend(backend: Arc<dyn DiagnosticBackend>) -> Self {
        Self { backend }
    }
}

async fn execute_step(
    backend: Arc<dyn DiagnosticBackend>,
    request: DiagnosticRequest,
) -> DiagnosticStep {
    let kind = request.kind();
    let started = Instant::now();
    let outcome = backend
        .execute(request)
        .await
        .map_err(|message| DiagnosticFailure { message });
    DiagnosticStep {
        kind,
        route: kind.route(),
        completed_at_ms: unix_time_ms(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        outcome,
    }
}

#[derive(Clone)]
pub(super) enum DiagnosticRequest {
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

impl DiagnosticRequest {
    fn kind(&self) -> DiagnosticStepKind {
        match self {
            Self::Controller => DiagnosticStepKind::Controller,
            Self::Capture => DiagnosticStepKind::Capture,
            Self::Dns {
                record_type: DnsRecordType::A,
                ..
            } => DiagnosticStepKind::DnsA,
            Self::Dns {
                record_type: DnsRecordType::Aaaa,
                ..
            } => DiagnosticStepKind::DnsAaaa,
            Self::Network {
                route: Some(NetworkProbeRoute::Direct),
                ..
            } => DiagnosticStepKind::NetworkDirect,
            Self::Network { .. } => DiagnosticStepKind::NetworkMihomo,
            Self::ProxyProviders => DiagnosticStepKind::ProxyProviders,
            Self::RuleProviders => DiagnosticStepKind::RuleProviders,
        }
    }
}

pub(super) type DiagnosticFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DiagnosticData, String>> + Send + 'a>>;

pub(super) trait DiagnosticBackend: Send + Sync {
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
