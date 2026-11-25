//! Capture plans, snapshots, outcomes, and snapshot inspection predicates.

use std::time::{SystemTime, UNIX_EPOCH};

use super::CaptureBackendSnapshot;
use crate::{
    Observation, RecoveryAction, SystemProxyOwnershipState, SystemProxySessionSnapshot,
    TunCaptureStatus,
};

/// Mutually exclusive capture choice offered by ordinary UI surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePlan {
    /// Disable ZenClash-owned System Proxy and TUN capture.
    Off,
    /// Capture through the native desktop System Proxy.
    SystemProxy,
    /// Configure the runtime TUN path after checking platform permission.
    Tun,
}

/// Capture combination observed without normalizing pre-existing state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedCapturePlan {
    /// Both System Proxy and TUN are observed or configured off.
    Off,
    /// Native System Proxy is active while TUN is configured off.
    SystemProxy,
    /// TUN is configured on while native System Proxy is off.
    TunConfigured,
    /// System Proxy and TUN are both present; the state is preserved until an explicit choice.
    Advanced,
    /// One or more required observations are unavailable.
    Unknown,
}

/// Read-only capture state returned by [`super::TrafficCaptureSession`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficCaptureSnapshot {
    /// System Proxy intent, native readback, and ownership.
    pub system_proxy: Observation<SystemProxySessionSnapshot>,
    /// TUN configuration, permission, and runtime activation fact.
    pub tun: Observation<TunCaptureStatus>,
    /// Combination derived from trustworthy System Proxy and TUN values.
    pub observed_plan: ObservedCapturePlan,
    /// HTTP/Mixed listener available for native proxy capture.
    pub system_proxy_port: Option<u16>,
    /// Whether the current runtime core is available.
    pub core_available: bool,
}

impl TrafficCaptureSnapshot {
    pub(super) fn from_backend(snapshot: CaptureBackendSnapshot) -> Self {
        let observed_at_ms = now_ms();
        let system_proxy = Observation::record(
            &Observation::Loading,
            snapshot.system_proxy.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let tun = Observation::record(
            &Observation::Loading,
            snapshot.tun.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let observed_plan = observed_capture_plan(&system_proxy, &tun);
        Self {
            system_proxy,
            tun,
            observed_plan,
            system_proxy_port: snapshot.system_proxy_port,
            core_available: snapshot.core_available,
        }
    }
}

/// Result of applying or releasing one capture plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The requested plan was applied; the snapshot remains authoritative for activation facts.
    Applied {
        /// Explicit user plan.
        plan: CapturePlan,
        /// Readback after the operation.
        snapshot: TrafficCaptureSnapshot,
    },
    /// The requested plan already matched current state.
    Unchanged {
        /// Current capture snapshot.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A later step failed and the earlier mutation was restored.
    RolledBack {
        /// Requested plan that did not complete.
        plan: CapturePlan,
        /// Original operation failure.
        failure: String,
        /// Readback after rollback.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A partial operation or failed rollback requires explicit reconciliation.
    ReconcileNeeded {
        /// Requested plan, when the outcome followed an apply operation.
        plan: Option<CapturePlan>,
        /// Combined operation and rollback failure.
        failure: String,
        /// Best-effort readback after the uncertain transition.
        snapshot: TrafficCaptureSnapshot,
    },
}

impl CaptureOutcome {
    /// Returns the latest read-only capture snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &TrafficCaptureSnapshot {
        match self {
            Self::Applied { snapshot, .. }
            | Self::Unchanged { snapshot }
            | Self::RolledBack { snapshot, .. }
            | Self::ReconcileNeeded { snapshot, .. } => snapshot,
        }
    }
}

/// Durable configuration receipt paired with the resulting capture observation.
#[derive(Clone, Debug)]
pub struct ServiceTunOutcome {
    pub(super) core: Option<crate::CoreApplyOutcome>,
    pub(super) capture: CaptureOutcome,
    pub(super) commit_pending: bool,
    pub(super) recovery_warning: Option<String>,
}

/// Frozen capture observation and the runtime result before native service maintenance.
pub struct ServiceLocalRecoveryOutcome {
    pub(super) before: TrafficCaptureSnapshot,
    pub(super) runtime: crate::CoreLocalRecoveryOutcome,
    pub(super) capture_revision: u64,
}

impl ServiceLocalRecoveryOutcome {
    pub(crate) const fn capture_revision(&self) -> u64 {
        self.capture_revision
    }

    /// Reads the capture facts held before owned native capture was released.
    #[must_use]
    pub const fn before(&self) -> &TrafficCaptureSnapshot {
        &self.before
    }

    /// Reads Local readiness and the scoped resources for a later service handover.
    #[must_use]
    pub const fn runtime(&self) -> &crate::CoreLocalRecoveryOutcome {
        &self.runtime
    }
}

impl ServiceTunOutcome {
    /// Returns a receipt only when business persistence succeeded, including pending finalization.
    #[must_use]
    pub const fn core(&self) -> Option<&crate::CoreApplyOutcome> {
        self.core.as_ref()
    }
    /// Returns capture success, confirmed rollback, or reconciliation needed after an uncertain result.
    #[must_use]
    pub const fn capture(&self) -> &CaptureOutcome {
        &self.capture
    }

    /// Reports an accepted configuration whose native commit still needs confirmation.
    #[must_use]
    pub const fn commit_pending(&self) -> bool {
        self.commit_pending
    }

    /// Reads a capture or cleanup warning that native commit confirmation cannot resolve.
    #[must_use]
    pub fn recovery_warning(&self) -> Option<&str> {
        self.recovery_warning.as_deref()
    }
}

pub(super) fn observed_capture_plan(
    system_proxy: &Observation<SystemProxySessionSnapshot>,
    tun: &Observation<TunCaptureStatus>,
) -> ObservedCapturePlan {
    let (Some(system_proxy), Some(tun)) = (system_proxy.value(), tun.value()) else {
        return ObservedCapturePlan::Unknown;
    };
    match (system_proxy.actual.active(), tun.configured) {
        (false, false) => ObservedCapturePlan::Off,
        (true, false) => ObservedCapturePlan::SystemProxy,
        (false, true) => ObservedCapturePlan::TunConfigured,
        (true, true) => ObservedCapturePlan::Advanced,
    }
}

pub(super) fn plan_matches(plan: CapturePlan, snapshot: &TrafficCaptureSnapshot) -> bool {
    match plan {
        CapturePlan::Off => {
            snapshot.observed_plan == ObservedCapturePlan::Off
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    !system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Unowned
                })
        }
        CapturePlan::SystemProxy => {
            snapshot.observed_plan == ObservedCapturePlan::SystemProxy
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Owned
                })
        }
        CapturePlan::Tun => snapshot.observed_plan == ObservedCapturePlan::TunConfigured,
    }
}

pub(super) fn has_external_system_proxy(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership != SystemProxyOwnershipState::Owned
    })
}

pub(super) fn system_proxy_is_owned_and_active(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

pub(super) fn system_proxy_has_intent_or_ownership(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.intent_enabled || system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

pub(super) fn tun_is_configured(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.tun.value().is_some_and(|tun| tun.configured)
}

pub(super) fn failed_snapshot(failure: String) -> TrafficCaptureSnapshot {
    let failure = crate::OperationalFailure {
        message: failure,
        occurred_at_ms: now_ms(),
    };
    TrafficCaptureSnapshot {
        system_proxy: Observation::Failed {
            failure: failure.clone(),
            recovery: RecoveryAction::ReviewCapture,
        },
        tun: Observation::Failed {
            failure,
            recovery: RecoveryAction::ReviewCapture,
        },
        observed_plan: ObservedCapturePlan::Unknown,
        system_proxy_port: None,
        core_available: false,
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
