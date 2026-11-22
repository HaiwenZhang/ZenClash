//! Background polling loop, platform TUN observation, and stream-freshness reducers.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use super::{
    CapabilityState, ControllerCompatibility, ControllerStatus, Observation, OperationalFailure,
    OperationalStatus, ProcessStatus, RecoveryAction, StreamStatus, StreamStatuses,
    TunCaptureStatus, memory, now_ms, types::aggregate_tun_state,
};
use crate::{
    CoreKind, CoreSession, CoreSessionSnapshot, LogMonitor, RuntimeConfig, SystemProxySession,
    TrafficMonitor,
};

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const PLATFORM_REFRESH_TICKS: u8 = 6;

#[derive(Debug, Default)]
pub(super) struct StatusRefreshSchedule {
    ticks_until_platform_refresh: u8,
}

impl StatusRefreshSchedule {
    pub(super) fn next_refreshes_platform(&mut self) -> bool {
        if self.ticks_until_platform_refresh == 0 {
            self.ticks_until_platform_refresh = PLATFORM_REFRESH_TICKS - 1;
            true
        } else {
            self.ticks_until_platform_refresh -= 1;
            false
        }
    }

    pub(super) fn request_platform_refresh(&mut self) {
        self.ticks_until_platform_refresh = 0;
    }
}

pub(super) async fn run_status_monitor(
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

pub(super) async fn refresh_status_with_tun_reader<F, R>(
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

pub(super) fn reset_old_generation_streams(streams: &mut StreamStatuses, generation: u64) {
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

pub(super) fn same_generation(expected: CoreSessionSnapshot, current: CoreSessionSnapshot) -> bool {
    expected.kind == current.kind && expected.generation == current.generation
}
