//! Read-only aggregation of runtime process, controller, capture, path, and stream facts.

use std::{
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::{runtime::Handle, sync::watch};

use crate::{
    CoreKind, CoreLifecyclePhase, CoreSession, CoreSessionSnapshot, LogMonitor, RuntimeConfig,
    SystemProxySession, SystemProxySessionSnapshot, TrafficMonitor, VersionInfo,
};

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const PLATFORM_REFRESH_TICKS: u8 = 6;

#[derive(Debug, Default)]
struct StatusRefreshSchedule {
    ticks_until_platform_refresh: u8,
}

impl StatusRefreshSchedule {
    fn next_refreshes_platform(&mut self) -> bool {
        if self.ticks_until_platform_refresh == 0 {
            self.ticks_until_platform_refresh = PLATFORM_REFRESH_TICKS - 1;
            true
        } else {
            self.ticks_until_platform_refresh -= 1;
            false
        }
    }
}

/// Recovery action associated with an observation that has no trustworthy value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the read without mutating runtime state.
    Retry,
    /// Inspect or restart the managed runtime core.
    InspectCore,
    /// Review native capture settings and ownership.
    ReviewCapture,
    /// No action is available because the selected core does not support the fact.
    Unsupported,
}

/// Failure attached to a stale or failed observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationalFailure {
    /// Human-readable failure suitable for diagnostics and localized UI wrapping.
    pub message: String,
    /// Unix timestamp in milliseconds when the failure was observed.
    pub occurred_at_ms: u64,
}

/// Trust state shared by every independently refreshed operational slice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Observation<T> {
    /// No successful value has been observed yet.
    #[default]
    Loading,
    /// A successful current value and its observation time.
    Fresh {
        /// Most recent successful value.
        value: T,
        /// Unix timestamp in milliseconds when the value was observed.
        observed_at_ms: u64,
    },
    /// The last successful value retained after a newer read failed.
    Stale {
        /// Last successful value.
        value: T,
        /// Unix timestamp in milliseconds of the last success.
        observed_at_ms: u64,
        /// Newer failure that made the value stale.
        failure: OperationalFailure,
    },
    /// A failed read for which no trustworthy value exists.
    Failed {
        /// Failure reported by the observer.
        failure: OperationalFailure,
        /// Safe next action offered to the caller.
        recovery: RecoveryAction,
    },
}

impl<T> Observation<T> {
    /// Returns the trustworthy current or last-successful value, if one exists.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Fresh { value, .. } | Self::Stale { value, .. } => Some(value),
            Self::Loading | Self::Failed { .. } => None,
        }
    }

    /// Returns the timestamp of the current or last-successful value.
    #[must_use]
    pub const fn observed_at_ms(&self) -> Option<u64> {
        match self {
            Self::Fresh { observed_at_ms, .. } | Self::Stale { observed_at_ms, .. } => {
                Some(*observed_at_ms)
            }
            Self::Loading | Self::Failed { .. } => None,
        }
    }

    /// Returns whether this observation contains a current successful value.
    #[must_use]
    pub const fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh { .. })
    }
}

impl<T: Clone> Observation<T> {
    /// Records an independent read while preserving a last-successful value on failure.
    #[must_use]
    pub fn record<E: ToString>(
        previous: &Self,
        result: Result<T, E>,
        observed_at_ms: u64,
        recovery: RecoveryAction,
    ) -> Self {
        match result {
            Ok(value) => Self::Fresh {
                value,
                observed_at_ms,
            },
            Err(error) => {
                let failure = OperationalFailure {
                    message: error.to_string(),
                    occurred_at_ms: observed_at_ms,
                };
                match previous {
                    Self::Fresh {
                        value,
                        observed_at_ms,
                    }
                    | Self::Stale {
                        value,
                        observed_at_ms,
                        ..
                    } => Self::Stale {
                        value: value.clone(),
                        observed_at_ms: *observed_at_ms,
                        failure,
                    },
                    Self::Loading | Self::Failed { .. } => Self::Failed { failure, recovery },
                }
            }
        }
    }

    /// Keeps an older trustworthy value when an independently loaded replacement failed.
    #[must_use]
    pub fn retain_last_success(previous: &Self, next: Self) -> Self {
        let Self::Failed { failure, recovery } = next else {
            return next;
        };
        match previous {
            Self::Fresh {
                value,
                observed_at_ms,
            }
            | Self::Stale {
                value,
                observed_at_ms,
                ..
            } => Self::Stale {
                value: value.clone(),
                observed_at_ms: *observed_at_ms,
                failure,
            },
            Self::Loading | Self::Failed { .. } => Self::Failed { failure, recovery },
        }
    }
}

/// Managed or external process facts associated with the active core session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessStatus {
    /// Concrete runtime core.
    pub kind: CoreKind,
    /// Whether ZenClash owns the child process.
    pub managed: bool,
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
    /// ZenClash observes an external core and cannot recover its process.
    External,
}

impl ProcessStatus {
    fn from_session(session: &CoreSession) -> Self {
        let snapshot = session.snapshot();
        let process = session.managed_process_snapshot();
        let lifecycle = session.lifecycle_snapshot();
        Self {
            kind: snapshot.kind,
            managed: snapshot.managed,
            running: snapshot.running,
            generation: snapshot.generation,
            exit_reason: lifecycle.exit_reason.or_else(|| {
                process
                    .as_ref()
                    .and_then(|process| process.exit_reason.clone())
            }),
            recovery_attempts: lifecycle.recovery_attempts,
            recovery: match lifecycle.phase {
                CoreLifecyclePhase::Stable => ProcessRecoveryStatus::Stable,
                CoreLifecyclePhase::Recovering => ProcessRecoveryStatus::Recovering,
                CoreLifecyclePhase::Failed => ProcessRecoveryStatus::Failed,
                CoreLifecyclePhase::ShuttingDown | CoreLifecyclePhase::Stopped => {
                    ProcessRecoveryStatus::Stopped
                }
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
    /// The selected experimental core exposes only its declared compatible subset.
    Limited,
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

/// Runtime support for one observable capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityState {
    /// The capability is observed inactive.
    Inactive,
    /// The capability is observed active.
    Active,
    /// Configuration is known but runtime activation cannot be verified.
    Unknown,
    /// The selected core does not claim this capability.
    Unsupported,
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
    fn from_config(kind: CoreKind, config: &RuntimeConfig) -> Self {
        if kind == CoreKind::Meow {
            return Self {
                requested: false,
                configured: false,
                permission: CapabilityState::Unsupported,
                runtime: crate::TunRuntimeObservation {
                    device_name: None,
                    device: CapabilityState::Unsupported,
                    route: CapabilityState::Unsupported,
                    detail: "meow-rs 未声明 TUN 能力".into(),
                },
                observed: CapabilityState::Unsupported,
            };
        }
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
        kind: CoreKind,
        config: &RuntimeConfig,
        permission: Option<&crate::TunPermissionStatus>,
    ) -> Self {
        if kind == CoreKind::Meow {
            return Self::from_config(kind, config);
        }
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

fn aggregate_tun_state(
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
    /// Returns true only for capture proven active by native or runtime readback.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.system_proxy
            .value()
            .is_some_and(|snapshot| snapshot.actual.active())
            || self
                .tun
                .value()
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

/// Next incomplete step derived from persistent and observed runtime facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRunStage {
    /// No managed profile is active yet.
    NoProfile,
    /// A profile exists, but the core/controller is not currently controllable.
    CoreUnavailable,
    /// No ZenClash-owned System Proxy or requested TUN plan exists.
    CaptureNotSelected,
    /// Capture was requested but its activation is not proven.
    CaptureUnconfirmed,
    /// Capture is active but no explicit path probe has completed.
    PathUnknown,
    /// The latest path probe failed or did not traverse Mihomo.
    PathFailed,
    /// Profile, runtime, capture, and path evidence are all present.
    Ready,
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

/// Independent traffic, log, and connection freshness.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamStatuses {
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

/// Receiver for changes to an [`OperationalSnapshot`].
pub type OperationalStatusStream = watch::Receiver<OperationalSnapshot>;

/// In-process owner of read-only operational facts.
pub struct OperationalStatus {
    snapshot: watch::Sender<OperationalSnapshot>,
}

impl OperationalStatus {
    /// Starts bounded polling over the supplied runtime owners.
    #[must_use]
    pub fn start(
        runtime: &Handle,
        core_session: CoreSession,
        system_proxy: Option<SystemProxySession>,
        tun_permissions: Option<crate::TunPermissionManager>,
        traffic: Arc<TrafficMonitor>,
        logs: Arc<LogMonitor>,
    ) -> Arc<Self> {
        let now = now_ms();
        let initial = OperationalSnapshot {
            process: Observation::Fresh {
                value: ProcessStatus::from_session(&core_session),
                observed_at_ms: now,
            },
            ..OperationalSnapshot::default()
        };
        let (snapshot, _) = watch::channel(initial);
        let status = Arc::new(Self { snapshot });
        let weak = Arc::downgrade(&status);
        runtime.spawn(run_status_monitor(
            weak,
            core_session,
            system_proxy,
            tun_permissions,
            traffic,
            logs,
        ));
        status
    }

    /// Returns a cheap point-in-time copy of every operational slice.
    #[must_use]
    pub fn snapshot(&self) -> OperationalSnapshot {
        self.snapshot.borrow().clone()
    }

    /// Subscribes to coherent snapshot replacements.
    #[must_use]
    pub fn subscribe(&self) -> OperationalStatusStream {
        self.snapshot.subscribe()
    }

    /// Records one explicit path-probe result for the expected core generation.
    ///
    /// Returns false when the core changed while the probe was in flight.
    pub fn record_path(
        &self,
        expected_generation: u64,
        result: Result<PathStatus, String>,
    ) -> bool {
        let mut applied = false;
        self.snapshot.send_modify(|snapshot| {
            let current_generation = snapshot
                .process
                .value()
                .map_or(0, |process| process.generation);
            if current_generation != expected_generation {
                return;
            }
            snapshot.path =
                Observation::record(&snapshot.path, result, now_ms(), RecoveryAction::Retry);
            applied = true;
        });
        applied
    }

    fn replace(&self, snapshot: OperationalSnapshot) {
        self.snapshot.send_replace(snapshot);
    }
}

impl OperationalSnapshot {
    /// Derives the next first-use step from facts rather than persisted wizard state.
    #[must_use]
    pub fn first_run_stage(&self, has_profile: bool) -> FirstRunStage {
        if !has_profile {
            return FirstRunStage::NoProfile;
        }
        let process_ready = self.process.value().is_some_and(|process| process.running);
        if !process_ready || self.controller.value().is_none() {
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
        let capture_active = system_proxy.is_some_and(|proxy| {
            proxy.actual.active() && proxy.ownership == crate::SystemProxyOwnershipState::Owned
        }) || tun.is_some_and(|tun| tun.observed == CapabilityState::Active);
        if !capture_active {
            return FirstRunStage::CaptureUnconfirmed;
        }
        match &self.path {
            Observation::Loading => FirstRunStage::PathUnknown,
            Observation::Fresh { value, .. }
                if value.route == ObservedPathRoute::Mihomo
                    && self
                        .process
                        .value()
                        .is_some_and(|process| value.generation == process.generation) =>
            {
                FirstRunStage::Ready
            }
            Observation::Fresh { .. } | Observation::Stale { .. } | Observation::Failed { .. } => {
                FirstRunStage::PathFailed
            }
        }
    }
}

async fn run_status_monitor(
    status: Weak<OperationalStatus>,
    core_session: CoreSession,
    system_proxy: Option<SystemProxySession>,
    tun_permissions: Option<crate::TunPermissionManager>,
    traffic: Arc<TrafficMonitor>,
    logs: Arc<LogMonitor>,
) {
    let mut schedule = StatusRefreshSchedule::default();
    while let Some(status) = status.upgrade() {
        let refresh_platform = schedule.next_refreshes_platform();
        refresh_status(
            &status,
            &core_session,
            system_proxy.clone(),
            tun_permissions.clone(),
            &traffic,
            &logs,
            refresh_platform,
        )
        .await;
        drop(status);
        tokio::time::sleep(STATUS_REFRESH_INTERVAL).await;
    }
}

async fn refresh_status(
    status: &OperationalStatus,
    core_session: &CoreSession,
    system_proxy: Option<SystemProxySession>,
    tun_permissions: Option<crate::TunPermissionManager>,
    traffic: &TrafficMonitor,
    logs: &LogMonitor,
    refresh_platform: bool,
) {
    let expected = core_session.snapshot();
    let client = core_session.client().clone();
    let native = async move {
        if !refresh_platform {
            return None;
        }
        Some(
            async move {
                let Some(session) = system_proxy else {
                    return Err("应用设置不可用，无法读取系统代理 intent".to_owned());
                };
                tokio::task::spawn_blocking(move || session.snapshot())
                    .await
                    .map_err(|error| format!("系统代理读取任务异常结束：{error}"))?
                    .map_err(|error| error.to_string())
            }
            .await,
        )
    };
    let (version, config, connections, native) = tokio::join!(
        client.version(),
        client.runtime_config(),
        client.connections_summary(),
        native,
    );
    let current = core_session.snapshot();
    let tun = if refresh_platform {
        Some(match &config {
            Ok(config) => {
                let config = config.clone();
                let kind = current.kind;
                tokio::task::spawn_blocking(move || {
                    let permission = tun_permissions
                        .as_ref()
                        .map(crate::TunPermissionManager::status)
                        .transpose()
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>(TunCaptureStatus::from_platform(
                        kind,
                        &config,
                        permission.as_ref(),
                    ))
                })
                .await
                .map_err(|error| format!("TUN 平台读取任务异常结束：{error}"))
                .and_then(|result| result)
            }
            Err(error) => Err(error.to_string()),
        })
    } else {
        None
    };
    traffic.synchronize_generation(current.generation);
    logs.synchronize_generation(current.generation);
    let now = now_ms();
    let mut next = status.snapshot();
    next.process = Observation::Fresh {
        value: ProcessStatus::from_session(core_session),
        observed_at_ms: now,
    };
    if !same_generation(expected, current) {
        reset_old_generation_streams(&mut next.streams, current.generation);
        status.replace(next);
        return;
    }

    next.controller = Observation::record(
        &next.controller,
        version.map(|version| ControllerStatus {
            version,
            authenticated: true,
            compatibility: if current.kind == CoreKind::Mihomo {
                ControllerCompatibility::Compatible
            } else {
                ControllerCompatibility::Limited
            },
            generation: current.generation,
        }),
        now,
        RecoveryAction::InspectCore,
    );
    if let Some(native) = native {
        next.capture.system_proxy = Observation::record(
            &next.capture.system_proxy,
            native,
            now,
            RecoveryAction::ReviewCapture,
        );
    }
    if let Some(tun) = tun {
        next.capture.tun = Observation::record(
            &next.capture.tun,
            tun,
            now,
            if current.kind == CoreKind::Meow {
                RecoveryAction::Unsupported
            } else {
                RecoveryAction::ReviewCapture
            },
        );
    }
    next.streams.traffic = observe_traffic(
        &generation_observation(&next.streams.traffic, current.generation),
        traffic,
        current.generation,
        now,
    );
    next.streams.logs = observe_logs(
        &generation_observation(&next.streams.logs, current.generation),
        logs,
        current.generation,
        now,
    );
    next.streams.connections = Observation::record(
        &generation_observation(&next.streams.connections, current.generation),
        connections.map(|snapshot| StreamStatus {
            generation: current.generation,
            last_success_at_ms: now,
            item_count: snapshot.active_connections,
            upload: snapshot.upload_total,
            download: snapshot.download_total,
            memory: snapshot.memory,
        }),
        now,
        RecoveryAction::Retry,
    );
    status.replace(next);
}

fn observe_traffic(
    previous: &Observation<StreamStatus>,
    monitor: &TrafficMonitor,
    generation: u64,
    now: u64,
) -> Observation<StreamStatus> {
    let snapshot = monitor.snapshot();
    if snapshot.generation != generation {
        return Observation::Loading;
    }
    let value = StreamStatus {
        generation,
        last_success_at_ms: snapshot.updated_at_ms,
        item_count: 0,
        upload: snapshot.upload,
        download: snapshot.download,
        memory: 0,
    };
    if snapshot.connected && snapshot.updated_at_ms > 0 {
        return Observation::Fresh {
            value,
            observed_at_ms: snapshot.updated_at_ms,
        };
    }
    let error = snapshot.last_error.unwrap_or_else(|| {
        if monitor.is_finished() {
            "流量监视任务已结束".into()
        } else {
            "流量流尚未连接".into()
        }
    });
    if snapshot.updated_at_ms > 0 {
        return Observation::Stale {
            value,
            observed_at_ms: snapshot.updated_at_ms,
            failure: OperationalFailure {
                message: error,
                occurred_at_ms: now,
            },
        };
    }
    Observation::record(previous, Err(error), now, RecoveryAction::Retry)
}

fn observe_logs(
    previous: &Observation<StreamStatus>,
    monitor: &LogMonitor,
    generation: u64,
    now: u64,
) -> Observation<StreamStatus> {
    let snapshot = monitor.stream_snapshot();
    if snapshot.generation != generation {
        return Observation::Loading;
    }
    let last_success_at_ms = snapshot.updated_at_ms;
    let value = StreamStatus {
        generation,
        last_success_at_ms,
        item_count: monitor.entry_count(),
        upload: 0,
        download: 0,
        memory: 0,
    };
    if snapshot.connected {
        return Observation::Fresh {
            value,
            observed_at_ms: if last_success_at_ms == 0 {
                now
            } else {
                last_success_at_ms
            },
        };
    }
    if last_success_at_ms > 0 {
        return Observation::Stale {
            value,
            observed_at_ms: last_success_at_ms,
            failure: OperationalFailure {
                message: snapshot
                    .last_error
                    .unwrap_or_else(|| "日志流已断开，正在重连".into()),
                occurred_at_ms: now,
            },
        };
    }
    Observation::record(
        previous,
        Err(snapshot.last_error.unwrap_or_else(|| {
            if monitor.is_finished() {
                "日志监视任务已结束".into()
            } else {
                "日志流尚未连接".into()
            }
        })),
        now,
        RecoveryAction::Retry,
    )
}

fn generation_observation<T>(previous: &Observation<T>, generation: u64) -> Observation<T>
where
    T: Clone + HasGeneration,
{
    if previous
        .value()
        .is_some_and(|value| value.generation() == generation)
    {
        previous.clone()
    } else {
        Observation::Loading
    }
}

trait HasGeneration {
    fn generation(&self) -> u64;
}

impl HasGeneration for StreamStatus {
    fn generation(&self) -> u64 {
        self.generation
    }
}

fn reset_old_generation_streams(streams: &mut StreamStatuses, generation: u64) {
    if streams
        .traffic
        .value()
        .is_some_and(|value| value.generation != generation)
    {
        streams.traffic = Observation::Loading;
    }
    if streams
        .logs
        .value()
        .is_some_and(|value| value.generation != generation)
    {
        streams.logs = Observation::Loading;
    }
    if streams
        .connections
        .value()
        .is_some_and(|value| value.generation != generation)
    {
        streams.connections = Observation::Loading;
    }
}

fn same_generation(expected: CoreSessionSnapshot, current: CoreSessionSnapshot) -> bool {
    expected.kind == current.kind && expected.generation == current.generation
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_refresh_schedule_probes_platform_on_the_first_tick() {
        let mut schedule = StatusRefreshSchedule::default();

        assert!(schedule.next_refreshes_platform());
    }

    #[test]
    fn status_refresh_schedule_limits_platform_probes_to_thirty_seconds() {
        let mut schedule = StatusRefreshSchedule::default();
        let probes = (0..12)
            .filter(|_| schedule.next_refreshes_platform())
            .collect::<Vec<_>>();

        assert_eq!(probes, vec![0, 6]);
    }
    use crate::{SystemProxyOwnershipState, SystemProxyStatus};

    #[test]
    fn failure_preserves_last_successful_value_and_timestamp_as_stale() {
        let previous = Observation::Fresh {
            value: 42,
            observed_at_ms: 10,
        };

        let observed = Observation::record(&previous, Err("offline"), 20, RecoveryAction::Retry);

        assert_eq!(
            observed,
            Observation::Stale {
                value: 42,
                observed_at_ms: 10,
                failure: OperationalFailure {
                    message: "offline".into(),
                    occurred_at_ms: 20,
                },
            }
        );
    }

    #[test]
    fn first_failure_has_no_invented_value_and_exposes_recovery() {
        let observed = Observation::<u8>::record(
            &Observation::Loading,
            Err("unauthorized"),
            20,
            RecoveryAction::InspectCore,
        );

        assert!(matches!(
            observed,
            Observation::Failed {
                recovery: RecoveryAction::InspectCore,
                ..
            }
        ));
        assert_eq!(observed.value(), None);
    }

    #[test]
    fn independently_loaded_failure_can_merge_with_a_previous_success() {
        let previous = Observation::Fresh {
            value: "profile-a",
            observed_at_ms: 10,
        };
        let next = Observation::Failed {
            failure: OperationalFailure {
                message: "controller offline".into(),
                occurred_at_ms: 20,
            },
            recovery: RecoveryAction::Retry,
        };

        assert!(matches!(
            Observation::retain_last_success(&previous, next),
            Observation::Stale {
                value: "profile-a",
                observed_at_ms: 10,
                ..
            }
        ));
    }

    #[test]
    fn traffic_activity_does_not_imply_capture() {
        let capture = CaptureStatus {
            system_proxy: Observation::Fresh {
                value: SystemProxySessionSnapshot {
                    intent_enabled: false,
                    actual: SystemProxyStatus::default(),
                    ownership: SystemProxyOwnershipState::Unowned,
                },
                observed_at_ms: 1,
            },
            tun: Observation::Fresh {
                value: TunCaptureStatus {
                    requested: false,
                    configured: false,
                    permission: CapabilityState::Unknown,
                    runtime: crate::TunRuntimeObserver::observe(&crate::TunConfig::default()),
                    observed: CapabilityState::Inactive,
                },
                observed_at_ms: 1,
            },
        };
        let traffic = Observation::Fresh {
            value: StreamStatus {
                generation: 1,
                last_success_at_ms: 1,
                upload: 1_024,
                download: 2_048,
                ..StreamStatus::default()
            },
            observed_at_ms: 1,
        };

        assert!(traffic.is_fresh());
        assert!(!capture.is_active());
    }

    #[test]
    fn configured_tun_is_unknown_until_runtime_activation_is_observed() {
        let config = RuntimeConfig {
            tun: crate::TunConfig {
                enable: true,
                ..crate::TunConfig::default()
            },
            ..RuntimeConfig::default()
        };

        let status = TunCaptureStatus::from_config(CoreKind::Mihomo, &config);

        assert!(status.requested);
        assert!(status.configured);
        assert_eq!(status.observed, CapabilityState::Unknown);
    }

    #[test]
    fn configured_tun_requires_permission_device_and_route_evidence() {
        let runtime = crate::TunRuntimeObservation {
            device_name: Some("Mihomo".into()),
            device: CapabilityState::Active,
            route: CapabilityState::Inactive,
            detail: "route uses another interface".into(),
        };

        assert_eq!(
            aggregate_tun_state(true, CapabilityState::Active, &runtime),
            CapabilityState::Inactive
        );
        assert_eq!(
            aggregate_tun_state(true, CapabilityState::Unknown, &runtime),
            CapabilityState::Inactive
        );
        let active_runtime = crate::TunRuntimeObservation {
            route: CapabilityState::Active,
            ..runtime
        };
        assert_eq!(
            aggregate_tun_state(true, CapabilityState::Active, &active_runtime),
            CapabilityState::Active
        );
    }

    #[test]
    fn meow_tun_status_is_explicitly_unsupported() {
        let status = TunCaptureStatus::from_config(CoreKind::Meow, &RuntimeConfig::default());

        assert_eq!(status.observed, CapabilityState::Unsupported);
    }

    #[test]
    fn async_result_from_an_old_generation_is_rejected() {
        let expected = CoreSessionSnapshot {
            kind: CoreKind::Mihomo,
            managed: true,
            running: true,
            generation: 3,
        };
        let current = CoreSessionSnapshot {
            generation: 4,
            ..expected
        };

        assert!(!same_generation(expected, current));
    }

    fn controllable_snapshot() -> OperationalSnapshot {
        OperationalSnapshot {
            process: Observation::Fresh {
                value: ProcessStatus {
                    kind: CoreKind::Mihomo,
                    managed: true,
                    running: true,
                    generation: 7,
                    exit_reason: None,
                    recovery_attempts: 0,
                    recovery: ProcessRecoveryStatus::Stable,
                },
                observed_at_ms: 1,
            },
            controller: Observation::Fresh {
                value: ControllerStatus {
                    version: VersionInfo {
                        meta: true,
                        version: "test".into(),
                    },
                    authenticated: true,
                    compatibility: ControllerCompatibility::Compatible,
                    generation: 7,
                },
                observed_at_ms: 1,
            },
            capture: CaptureStatus {
                system_proxy: Observation::Fresh {
                    value: SystemProxySessionSnapshot {
                        intent_enabled: false,
                        actual: SystemProxyStatus::default(),
                        ownership: SystemProxyOwnershipState::Unowned,
                    },
                    observed_at_ms: 1,
                },
                tun: Observation::Fresh {
                    value: TunCaptureStatus {
                        requested: false,
                        configured: false,
                        permission: CapabilityState::Unknown,
                        runtime: crate::TunRuntimeObserver::observe(&crate::TunConfig::default()),
                        observed: CapabilityState::Inactive,
                    },
                    observed_at_ms: 1,
                },
            },
            ..OperationalSnapshot::default()
        }
    }

    #[test]
    fn first_run_resumes_from_domain_facts_without_a_persisted_wizard_step() {
        let mut snapshot = controllable_snapshot();
        assert_eq!(snapshot.first_run_stage(false), FirstRunStage::NoProfile);
        assert_eq!(
            snapshot.first_run_stage(true),
            FirstRunStage::CaptureNotSelected
        );

        let system_proxy = snapshot.capture.system_proxy.value().unwrap().clone();
        snapshot.capture.system_proxy = Observation::Fresh {
            value: SystemProxySessionSnapshot {
                intent_enabled: true,
                ..system_proxy
            },
            observed_at_ms: 2,
        };
        assert_eq!(
            snapshot.first_run_stage(true),
            FirstRunStage::CaptureUnconfirmed
        );

        let system_proxy = snapshot.capture.system_proxy.value().unwrap().clone();
        snapshot.capture.system_proxy = Observation::Fresh {
            value: SystemProxySessionSnapshot {
                actual: SystemProxyStatus {
                    enabled: true,
                    secure_enabled: true,
                    port: 7890,
                    secure_port: 7890,
                    ..SystemProxyStatus::default()
                },
                ownership: SystemProxyOwnershipState::Owned,
                ..system_proxy
            },
            observed_at_ms: 3,
        };
        assert_eq!(snapshot.first_run_stage(true), FirstRunStage::PathUnknown);

        snapshot.path = Observation::Fresh {
            value: PathStatus {
                route: ObservedPathRoute::Mihomo,
                target: "https://example.com".into(),
                generation: 7,
            },
            observed_at_ms: 4,
        };
        assert_eq!(snapshot.first_run_stage(true), FirstRunStage::Ready);
    }

    #[test]
    fn old_generation_path_probe_cannot_advance_first_run_state() {
        let (sender, _) = watch::channel(controllable_snapshot());
        let status = OperationalStatus { snapshot: sender };

        assert!(!status.record_path(
            6,
            Ok(PathStatus {
                route: ObservedPathRoute::Mihomo,
                target: "https://example.com".into(),
                generation: 6,
            })
        ));
        assert!(matches!(status.snapshot().path, Observation::Loading));
    }
}
