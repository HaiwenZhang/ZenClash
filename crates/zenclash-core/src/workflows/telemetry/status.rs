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
mod memory;
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

    fn request_platform_refresh(&mut self) {
        self.ticks_until_platform_refresh = 0;
    }
}

pub use crate::platform::observation::{
    CapabilityState, FirstRunStage, Observation, OperationalFailure, RecoveryAction,
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

    fn from_snapshot(session: &CoreSession, snapshot: CoreSessionSnapshot) -> Self {
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
    fn from_config(_kind: CoreKind, config: &RuntimeConfig) -> Self {
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

/// Receiver for changes to an [`OperationalSnapshot`].
pub type OperationalStatusStream = watch::Receiver<OperationalSnapshot>;

/// In-process owner of read-only operational facts.
pub struct OperationalStatus {
    snapshot: watch::Sender<OperationalSnapshot>,
    task: std::sync::OnceLock<tokio::task::AbortHandle>,
    wake: Arc<tokio::sync::Notify>,
    core_session: Option<CoreSession>,
}

impl Drop for OperationalStatus {
    fn drop(&mut self) {
        self.stop();
    }
}

impl OperationalStatus {
    /// Starts bounded polling over the supplied runtime owners.
    #[must_use]
    pub fn start(
        runtime: &Handle,
        core_session: CoreSession,
        system_proxy: Option<SystemProxySession>,
        traffic: Arc<TrafficMonitor>,
        logs: Arc<LogMonitor>,
    ) -> Arc<Self> {
        let initial = OperationalSnapshot::default();
        let (snapshot, _) = watch::channel(initial);
        let wake = Arc::new(tokio::sync::Notify::new());
        let status = Arc::new(Self {
            snapshot,
            task: std::sync::OnceLock::new(),
            wake: wake.clone(),
            core_session: Some(core_session.clone()),
        });
        let weak = Arc::downgrade(&status);
        let task = runtime.spawn(run_status_monitor(
            weak,
            core_session,
            system_proxy,
            traffic,
            logs,
            wake,
        ));
        let _ = status.task.set(task.abort_handle());
        status
    }

    /// Wakes the background status monitor to perform an immediate platform and TUN refresh.
    pub fn request_platform_refresh(&self) {
        self.wake.notify_one();
    }

    /// Cancels outstanding sampling immediately when its owning target is closed.
    pub fn stop(&self) {
        if let Some(task) = self.task.get() {
            task.abort();
        }
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
            let current_generation = match &snapshot.process {
                Observation::Fresh { value, .. } | Observation::Stale { value, .. } => {
                    Some(value.generation)
                }
                Observation::Loading => self.core_session.as_ref().map(CoreSession::generation),
                Observation::Failed { .. } => None,
            };
            if current_generation != Some(expected_generation) {
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

async fn run_status_monitor(
    status: Weak<OperationalStatus>,
    core_session: CoreSession,
    system_proxy: Option<SystemProxySession>,
    traffic: Arc<TrafficMonitor>,
    logs: Arc<LogMonitor>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut schedule = StatusRefreshSchedule::default();
    let mut binding_updates = core_session.client().binding.subscribe();
    let mut run_state_updates = core_session.subscribe_run_state();
    let capture_intent = core_session.capture_intent_revision();
    let mut last_generation = core_session.generation();
    let mut last_capture_intent = capture_intent.load(std::sync::atomic::Ordering::SeqCst);

    while let Some(status) = status.upgrade() {
        let current_generation = core_session.generation();
        let current_capture_intent = capture_intent.load(std::sync::atomic::Ordering::SeqCst);
        let tun_loading = matches!(status.snapshot().capture.tun, Observation::Loading);
        if current_generation != last_generation
            || current_capture_intent != last_capture_intent
            || tun_loading
        {
            schedule.request_platform_refresh();
            last_generation = current_generation;
            last_capture_intent = current_capture_intent;
        }
        let _ = binding_updates.borrow_and_update();
        let _ = run_state_updates.borrow_and_update();
        let refresh_platform = schedule.next_refreshes_platform();
        Box::pin(refresh_status(
            &status,
            &core_session,
            system_proxy.clone(),
            &traffic,
            &logs,
            refresh_platform,
        ))
        .await;
        let post_generation = core_session.generation();
        let post_capture_intent = capture_intent.load(std::sync::atomic::Ordering::SeqCst);
        let needs_immediate_retry =
            post_generation != last_generation || post_capture_intent != last_capture_intent;
        drop(status);
        if needs_immediate_retry {
            schedule.request_platform_refresh();
            continue;
        }
        tokio::select! {
            () = tokio::time::sleep(STATUS_REFRESH_INTERVAL) => {}
            () = wake.notified() => {
                schedule.request_platform_refresh();
            }
            result = binding_updates.changed() => {
                if result.is_err() {
                    break;
                }
                schedule.request_platform_refresh();
            }
            result = run_state_updates.changed() => {
                if result.is_err() {
                    break;
                }
                schedule.request_platform_refresh();
            }
        }
    }
}

async fn refresh_status(
    status: &OperationalStatus,
    core_session: &CoreSession,
    system_proxy: Option<SystemProxySession>,
    traffic: &TrafficMonitor,
    logs: &LogMonitor,
    refresh_platform: bool,
) {
    Box::pin(refresh_status_with_tun_reader(
        status,
        core_session,
        system_proxy,
        traffic,
        logs,
        refresh_platform,
        move |kind, config| observe_runtime_tun(core_session, kind, config),
    ))
    .await;
}

pub(crate) async fn observe_runtime_tun(
    session: &CoreSession,
    kind: CoreKind,
    config: RuntimeConfig,
) -> Result<TunCaptureStatus, String> {
    if !config.tun.enable {
        return tokio::task::spawn_blocking(move || {
            TunCaptureStatus::from_platform(kind, &config, None)
        })
        .await
        .map_err(|error| error.to_string());
    }
    let evidence = session
        .tun_permission_status()
        .await
        .map_err(|error| error.to_string())?;
    let Observation::Fresh {
        value: permission, ..
    } = evidence
    else {
        return Err(zenclash_i18n::text(
            "core_page.errors.runtime_authority_unavailable",
        ));
    };
    tokio::task::spawn_blocking(move || {
        let runtime = crate::TunRuntimeObserver::observe(&config.tun);
        let permission = if permission.granted() {
            CapabilityState::Active
        } else {
            CapabilityState::Inactive
        };
        TunCaptureStatus {
            requested: config.tun.enable,
            configured: config.tun.enable,
            observed: aggregate_tun_state(config.tun.enable, permission, &runtime),
            permission,
            runtime,
        }
    })
    .await
    .map_err(|error| error.to_string())
}

async fn refresh_status_with_tun_reader<F, R>(
    status: &OperationalStatus,
    core_session: &CoreSession,
    system_proxy: Option<SystemProxySession>,
    traffic: &TrafficMonitor,
    logs: &LogMonitor,
    refresh_platform: bool,
    read_tun: F,
) where
    F: FnOnce(CoreKind, RuntimeConfig) -> R,
    R: Future<Output = Result<TunCaptureStatus, String>>,
{
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
    let pid = core_session
        .managed_process_snapshot()
        .and_then(|process| process.pid)
        .or_else(|| {
            client
                .service_client()
                .and_then(|client| client.snapshot())
                .and_then(|snapshot| snapshot.core_pid)
        });
    let config_read = async {
        if !refresh_platform {
            return None;
        }
        Some(client.runtime_config().await)
    };
    let (version, config, connections, native) = tokio::join!(
        client.version(),
        config_read,
        client.connections_summary(),
        native,
    );
    // Reuse memory already reported by `/connections` before opening a dedicated
    // `/memory` WebSocket or probing OS resident memory.
    let memory = if let Ok(snapshot) = &connections
        && snapshot.memory > 0
    {
        Ok(crate::MemorySnapshot {
            inuse: snapshot.memory,
            oslimit: 0,
        })
    } else {
        let reported = client.memory_snapshot().await;
        match reported {
            Ok(memory) if memory.inuse > 0 => Ok(memory),
            reported => {
                if let Some(pid) = pid
                    && let Ok(Some(bytes)) =
                        tokio::task::spawn_blocking(move || memory::resident_memory(pid)).await
                {
                    Ok(crate::MemorySnapshot {
                        inuse: bytes,
                        oslimit: 0,
                    })
                } else {
                    match reported {
                        Ok(_) => Err(crate::MihomoError::Process(
                            "Core memory is unavailable".into(),
                        )),
                        Err(error) => Err(error),
                    }
                }
            }
        }
    };
    let tun = if let Some(config) = config {
        Some(match config {
            Ok(config) => read_tun(expected.kind, config).await,
            Err(error) => Err(error.to_string()),
        })
    } else {
        None
    };
    core_session
        .observe_current(|current| {
            traffic.synchronize_generation(current.generation);
            logs.synchronize_generation(current.generation);
            let now = now_ms();
            let mut next = status.snapshot();
            if !next.process.value().is_some_and(|previous| {
                previous.kind == current.kind && previous.generation == current.generation
            }) {
                reset_old_generation_streams(&mut next.streams, current.generation);
                next.controller = Observation::Loading;
                next.capture.tun = Observation::Loading;
                next.path = Observation::Loading;
            }
            next.process = if current.managed && current.running.is_none() {
                Observation::Failed {
                    failure: OperationalFailure {
                        message: zenclash_i18n::text(
                            "core_page.errors.runtime_authority_unavailable",
                        ),
                        occurred_at_ms: now,
                    },
                    recovery: RecoveryAction::InspectCore,
                }
            } else {
                let mut value = ProcessStatus::from_snapshot(core_session, current);
                if !current.managed {
                    value.running = version.is_ok();
                }
                Observation::Fresh {
                    value,
                    observed_at_ms: now,
                }
            };
            if !same_generation(expected, current) {
                reset_old_generation_streams(&mut next.streams, current.generation);
                next.controller = Observation::Loading;
                next.capture.tun = Observation::Loading;
                next.path = Observation::Loading;
                status.replace(next);
                return;
            }

            next.controller = Observation::record(
                &next.controller,
                version.map(|version| ControllerStatus {
                    version,
                    authenticated: true,
                    compatibility: ControllerCompatibility::Compatible,
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
                next.capture.tun =
                    Observation::record(&next.capture.tun, tun, now, RecoveryAction::ReviewCapture);
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
            next.streams.memory = Observation::record(
                &generation_observation(&next.streams.memory, current.generation),
                memory.map(|memory| StreamStatus {
                    generation: current.generation,
                    last_success_at_ms: now,
                    memory: memory.inuse,
                    ..Default::default()
                }),
                now,
                RecoveryAction::Retry,
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
        })
        .await;
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

fn generation_observation(
    previous: &Observation<StreamStatus>,
    generation: u64,
) -> Observation<StreamStatus> {
    if previous
        .value()
        .is_some_and(|value| value.generation == generation)
    {
        previous.clone()
    } else {
        Observation::Loading
    }
}

fn reset_old_generation_streams(streams: &mut StreamStatuses, generation: u64) {
    for stream in [
        &mut streams.memory,
        &mut streams.traffic,
        &mut streams.logs,
        &mut streams.connections,
    ] {
        if stream
            .value()
            .is_some_and(|value| value.generation != generation)
        {
            *stream = Observation::Loading;
        }
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
mod tests;
