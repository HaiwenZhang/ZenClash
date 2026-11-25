//! Operational status snapshot data types and pure state-derivation helpers.

use crate::{
    CapabilityState, CoreKind, CoreLifecyclePhase, CoreSession, CoreSessionSnapshot, FirstRunStage,
    Observation, RuntimeConfig, SystemProxySessionSnapshot, VersionInfo,
};

/// Managed or external process facts associated with the active core session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessStatus {
    /// Concrete runtime core.
    pub kind: CoreKind,
    /// Whether ZenClash owns the child process.
    pub managed: bool,
    /// Last observed live process identifier for a managed core.
    pub pid: Option<u32>,
    /// Unix timestamp of the observed live managed core's successful start.
    /// External attachments and unavailable observations have no start time.
    pub started_at_secs: Option<u64>,
    /// Whether the process or external attachment is currently running.
    pub running: bool,
    /// Last successful runtime transition generation.
    pub generation: u64,
    /// Last known exit reason, when one was captured.
    pub exit_reason: Option<String>,
    /// Number of automatic recovery attempts in the current process generation.
    pub recovery_attempts: u32,
    /// Current recovery requirement for the process layer.
    pub recovery: ProcessRecoveryStatus,
}

/// Recovery state derived from managed-process ownership and liveness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessRecoveryStatus {
    /// The managed process is healthy and needs no recovery.
    Stable,
    /// The managed process exited and a bounded restart is in progress.
    Recovering,
    /// Recovery attempts were exhausted and user action is required.
    Failed,
    /// An explicit application shutdown is in progress or complete.
    Stopped,
    /// Link loss paused the process and network recovery may resume it.
    NetworkSuspended,
    /// ZenClash observes an external core and cannot recover its process.
    External,
    /// Native managed liveness is unavailable or lifecycle acknowledgement is uncertain.
    Unknown,
}

impl ProcessStatus {
    /// Returns elapsed seconds only for a live managed core with a valid start time.
    #[must_use]
    pub fn uptime_secs_at(&self, now_secs: u64) -> Option<u64> {
        if !self.managed || !self.running {
            return None;
        }
        self.started_at_secs
            .filter(|started| *started > 0)
            .and_then(|started| now_secs.checked_sub(started))
    }

    pub(super) fn from_snapshot(session: &CoreSession, snapshot: CoreSessionSnapshot) -> Self {
        let process = session.managed_process_snapshot();
        let lifecycle = session.lifecycle_snapshot();
        Self {
            kind: snapshot.kind,
            managed: snapshot.managed,
            pid: process
                .as_ref()
                .and_then(|process| process.pid)
                .or_else(|| {
                    session
                        .client()
                        .service_client()
                        .and_then(|client| client.snapshot())
                        .and_then(|status| status.core_pid)
                }),
            running: snapshot.running.unwrap_or(false),
            started_at_secs: snapshot.running.filter(|running| *running).and_then(|_| {
                process
                    .as_ref()
                    .and_then(|process| process.started_at_secs)
                    .or_else(|| {
                        session
                            .client()
                            .service_client()
                            .and_then(|client| client.snapshot())
                            .and_then(|status| status.core_started_at)
                    })
            }),
            generation: snapshot.generation,
            exit_reason: lifecycle.exit_reason.or_else(|| {
                process
                    .as_ref()
                    .and_then(|process| process.exit_reason.clone())
            }),
            recovery_attempts: lifecycle.recovery_attempts,
            recovery: match lifecycle.phase {
                CoreLifecyclePhase::Stable => ProcessRecoveryStatus::Stable,
                CoreLifecyclePhase::Unknown => ProcessRecoveryStatus::Unknown,
                CoreLifecyclePhase::Recovering => ProcessRecoveryStatus::Recovering,
                CoreLifecyclePhase::Failed => ProcessRecoveryStatus::Failed,
                CoreLifecyclePhase::ShuttingDown | CoreLifecyclePhase::Stopped => {
                    ProcessRecoveryStatus::Stopped
                }
                CoreLifecyclePhase::NetworkSuspended => ProcessRecoveryStatus::NetworkSuspended,
                CoreLifecyclePhase::External => ProcessRecoveryStatus::External,
            },
        }
    }
}

/// Compatibility reported for a successful controller response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerCompatibility {
    /// The selected core exposes the expected production Mihomo protocol.
    Compatible,
}

/// Successful controller observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerStatus {
    /// Version response proving that the configured secret was accepted.
    pub version: VersionInfo,
    /// Whether the configured controller credentials were accepted.
    pub authenticated: bool,
    /// Protocol compatibility declared for the selected core.
    pub compatibility: ControllerCompatibility,
    /// Core-session generation to which this response belongs.
    pub generation: u64,
}

/// Requested, configured, and observed TUN state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunCaptureStatus {
    /// Whether the effective runtime configuration requests TUN.
    pub requested: bool,
    /// Whether the controller reports TUN configured on.
    pub configured: bool,
    /// Effective privilege state for the selected core executable.
    pub permission: CapabilityState,
    /// Native virtual-interface and route readback.
    pub runtime: crate::TunRuntimeObservation,
    /// Runtime activation fact; configured-on alone never implies active.
    pub observed: CapabilityState,
}

impl TunCaptureStatus {
    #[cfg(test)]
    pub(super) fn from_config(_kind: CoreKind, config: &RuntimeConfig) -> Self {
        let runtime = if config.tun.enable {
            crate::TunRuntimeObservation {
                device_name: (!config.tun.device.trim().is_empty())
                    .then(|| config.tun.device.trim().to_owned()),
                device: CapabilityState::Unknown,
                route: CapabilityState::Unknown,
                detail: "尚未读取平台 TUN 设备和路由".into(),
            }
        } else {
            crate::TunRuntimeObserver::observe(&config.tun)
        };
        Self {
            requested: config.tun.enable,
            configured: config.tun.enable,
            permission: CapabilityState::Unknown,
            runtime,
            observed: if config.tun.enable {
                CapabilityState::Unknown
            } else {
                CapabilityState::Inactive
            },
        }
    }

    pub(crate) fn from_platform(
        _kind: CoreKind,
        config: &RuntimeConfig,
        permission: Option<&crate::TunPermissionStatus>,
    ) -> Self {
        let runtime = crate::TunRuntimeObserver::observe(&config.tun);
        let permission = permission.map_or(CapabilityState::Unknown, |status| {
            if status.granted {
                CapabilityState::Active
            } else {
                CapabilityState::Inactive
            }
        });
        let observed = aggregate_tun_state(config.tun.enable, permission, &runtime);
        Self {
            requested: config.tun.enable,
            configured: config.tun.enable,
            permission,
            runtime,
            observed,
        }
    }
}

pub(super) fn aggregate_tun_state(
    configured: bool,
    permission: CapabilityState,
    runtime: &crate::TunRuntimeObservation,
) -> CapabilityState {
    if !configured {
        return CapabilityState::Inactive;
    }
    let evidence = [permission, runtime.device, runtime.route];
    if evidence.contains(&CapabilityState::Inactive) {
        CapabilityState::Inactive
    } else if evidence.contains(&CapabilityState::Unknown) {
        CapabilityState::Unknown
    } else if evidence.contains(&CapabilityState::Unsupported) {
        CapabilityState::Unsupported
    } else {
        CapabilityState::Active
    }
}

/// Independent native and TUN capture observations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureStatus {
    /// System Proxy intent, native readback, and ownership.
    pub system_proxy: Observation<SystemProxySessionSnapshot>,
    /// TUN request, configuration, and runtime observation.
    pub tun: Observation<TunCaptureStatus>,
}

impl CaptureStatus {
    /// Returns true only for ZenClash-owned capture proven active by native or runtime readback.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.system_proxy.fresh_value().is_some_and(|snapshot| {
            snapshot.actual.active()
                && snapshot.ownership == crate::SystemProxyOwnershipState::Owned
        }) || self
            .tun
            .fresh_value()
            .is_some_and(|snapshot| snapshot.observed == CapabilityState::Active)
    }
}

/// Network route used by the last explicit path probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedPathRoute {
    /// The probe explicitly used Mihomo.
    Mihomo,
    /// The probe explicitly bypassed Mihomo.
    Direct,
}

/// Last explicit network-path observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathStatus {
    /// Route used by the probe.
    pub route: ObservedPathRoute,
    /// Probe target shown to the user.
    pub target: String,
    /// Core-session generation that produced this explicit path observation.
    pub generation: u64,
}

/// Last successful observation for a runtime stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStatus {
    /// Core-session generation that produced the value.
    pub generation: u64,
    /// Unix timestamp in milliseconds of the last successful frame or response.
    pub last_success_at_ms: u64,
    /// Number of current items, when the stream exposes a collection.
    pub item_count: usize,
    /// Current upload rate or aggregate bytes, depending on the stream.
    pub upload: u64,
    /// Current download rate or aggregate bytes, depending on the stream.
    pub download: u64,
    /// Memory usage reported by `/connections`, or zero for other streams.
    pub memory: u64,
}

/// Independent memory, traffic, log, and connection freshness.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamStatuses {
    /// Current resident memory from the owned process or `/memory` for external cores.
    pub memory: Observation<StreamStatus>,
    /// `/traffic` WebSocket freshness.
    pub traffic: Observation<StreamStatus>,
    /// `/logs` WebSocket freshness.
    pub logs: Observation<StreamStatus>,
    /// `/connections` response freshness.
    pub connections: Observation<StreamStatus>,
}

/// Point-in-time operational truth assembled from independent observers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationalSnapshot {
    /// L1 process ownership and lifecycle.
    pub process: Observation<ProcessStatus>,
    /// L2 controller authentication, compatibility, and generation.
    pub controller: Observation<ControllerStatus>,
    /// L3 native System Proxy and TUN capture.
    pub capture: CaptureStatus,
    /// L4 last explicit route probe.
    pub path: Observation<PathStatus>,
    /// Live stream freshness.
    pub streams: StreamStatuses,
}

impl OperationalSnapshot {
    /// Derives the next first-use step from facts rather than persisted wizard state.
    #[must_use]
    pub fn first_run_stage(&self, has_profile: bool) -> FirstRunStage {
        if !has_profile {
            return FirstRunStage::NoProfile;
        }
        let Some(process) = self.process.fresh_value().filter(|process| process.running) else {
            return FirstRunStage::CoreUnavailable;
        };
        if !self.controller.fresh_value().is_some_and(|controller| {
            controller.authenticated && controller.generation == process.generation
        }) {
            return FirstRunStage::CoreUnavailable;
        }
        let system_proxy = self.capture.system_proxy.value();
        let tun = self.capture.tun.value();
        let capture_selected = system_proxy.is_some_and(|proxy| {
            proxy.intent_enabled || proxy.ownership == crate::SystemProxyOwnershipState::Owned
        }) || tun.is_some_and(|tun| tun.requested || tun.configured);
        if !capture_selected {
            return FirstRunStage::CaptureNotSelected;
        }
        if !self.capture.is_active() {
            return FirstRunStage::CaptureUnconfirmed;
        }
        match &self.path {
            Observation::Loading => FirstRunStage::PathUnknown,
            Observation::Fresh { value, .. }
                if value.route == ObservedPathRoute::Mihomo
                    && value.generation == process.generation =>
            {
                FirstRunStage::Ready
            }
            Observation::Fresh { .. } | Observation::Stale { .. } | Observation::Failed { .. } => {
                FirstRunStage::PathFailed
            }
        }
    }
}
