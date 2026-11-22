//! Network probes, multi-step diagnostics runner, application release checks, and AI network helpers.

/// Local AI network probe and reachability helpers.
pub mod ai_network;
pub(crate) mod app_update;
pub(crate) mod network;
pub(crate) mod runner;

pub use app_update::{
    AppRelease, AppUpdateError, AppUpdateResult, AppUpdateService, AppUpdateStatus,
    validate_external_https_url,
};
pub use network::{
    DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyResult, NetworkLatencyTarget, NetworkProbeError,
    NetworkProbeResult, NetworkProbeRoute, NetworkProbeService, NetworkProbeSnapshot,
    NetworkReachability, PublicIpInfo, PublicIpProvider, SystemNetworkSnapshot,
};
pub use runner::{
    DiagnosticData, DiagnosticFailure, DiagnosticPlan, DiagnosticReport, DiagnosticRoute,
    DiagnosticStep, DiagnosticStepKind, NetworkDiagnostics, NetworkDiagnosticsError,
    NetworkDiagnosticsResult, SupportBundle, SupportSafe,
};
