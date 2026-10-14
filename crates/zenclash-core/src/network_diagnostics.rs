//! Route-aware diagnostic plans/results, independent execution, and support-safe export.

use serde::Serialize;
use thiserror::Error;

use crate::{
    CaptureStatus, DnsQueryResponse, NetworkLatencyTarget, NetworkProbeRoute, NetworkProbeSnapshot,
    ProviderCatalog, PublicIpProvider, VersionInfo,
};

mod execution;
mod support_export;

pub use execution::NetworkDiagnostics;
pub use support_export::{SupportBundle, SupportSafe};

/// Route or subsystem responsible for one diagnostic step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticRoute {
    /// Mihomo's authenticated external controller.
    Controller,
    /// Read-only state owned by the local application.
    Local,
    /// HTTP requests without a configured or ambient HTTP proxy; OS routing still applies.
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
    /// Public-IP and latency requests without an HTTP proxy.
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
    /// Returns the normalized DNS name submitted for this run.
    #[must_use]
    pub fn dns_name(&self) -> &str {
        &self.dns_name
    }

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

fn unix_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
