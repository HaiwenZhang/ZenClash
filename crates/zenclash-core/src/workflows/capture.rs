//! Traffic-capture session orchestration and native desktop system-proxy management.

pub(crate) mod session;
pub(crate) mod system_proxy;

pub use session::{
    CaptureOutcome, CapturePlan, ObservedCapturePlan, ServiceLocalRecoveryOutcome,
    ServiceTunOutcome, TrafficCaptureSession, TrafficCaptureSnapshot,
};
pub use system_proxy::{
    PacProxyStatus, PacServer, PacServerStatus, ProxyEndpointStatus, SystemProxyConfiguration,
    SystemProxyController, SystemProxyManager, SystemProxyOperation, SystemProxyOwnershipState,
    SystemProxyReconcileOutcome, SystemProxyReleaseReason, SystemProxySession,
    SystemProxySessionError, SystemProxySessionResult, SystemProxySessionSnapshot,
    SystemProxySettings, SystemProxyStatus, SystemProxySummary,
};
