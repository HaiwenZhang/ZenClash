// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Core process watchdog supervision, restart backoff, and runtime record persistence.

use super::CoreManager;
use super::ipc_security::prepare_core_ipc_socket;
use super::process_guard::{ChildGuard, core_args, log_core_exit, run_with_logging, term_grace};
use crate::OwnerIdentity;
use crate::core::ClashConfig;
use crate::core::process::process_identity;
use crate::core::runtime::{CoreRuntimeRecord, remove_core_runtime_record, write_core_runtime_record};
use crate::core::state::set_core_lifecycle_state;
use crate::core::structure::ServiceLifecycleState;
use anyhow::{Context as _, Result, anyhow};
#[cfg(feature = "test")]
use parking_lot::Mutex as SyncMutex;
#[cfg(feature = "test")]
use std::sync::LazyLock;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use tracing::{error, info, warn};

#[derive(Clone, Copy)]
struct WatchdogConfig {
    max_restarts: u32,
    restart_window: Duration,
    max_backoff: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            max_restarts: 10,
            restart_window: Duration::from_secs(600),
            max_backoff: Duration::from_secs(30),
        }
    }
}

#[cfg(feature = "test")]
#[derive(Clone, Copy)]
pub struct CoreWatchdogTestConfig {
    pub max_restarts: u32,
    pub restart_window: Duration,
    pub max_backoff: Duration,
}

#[cfg(feature = "test")]
static WATCHDOG_CONFIG_OVERRIDE: LazyLock<SyncMutex<Option<WatchdogConfig>>> = LazyLock::new(|| SyncMutex::new(None));

/// Sets the isolated test watchdog override.
#[cfg(feature = "test")]
pub fn set_core_watchdog_config_for_tests(config: Option<CoreWatchdogTestConfig>) {
    let mut guard = WATCHDOG_CONFIG_OVERRIDE.lock();
    *guard = config.map(|config| WatchdogConfig {
        max_restarts: config.max_restarts,
        restart_window: config.restart_window,
        max_backoff: config.max_backoff,
    });
}

fn watchdog_config() -> WatchdogConfig {
    #[cfg(feature = "test")]
    if let Some(config) = *WATCHDOG_CONFIG_OVERRIDE.lock() {
        return config;
    }

    WatchdogConfig::default()
}

fn backoff_delay(attempt: u32, max: Duration) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }

    let base = Duration::from_secs(1u64 << (attempt - 1).min(5));
    base.min(max)
}

pub(super) fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub(super) async fn write_runtime_record_for_config(
    pid: Option<u32>,
    config: &ClashConfig,
    context: &'static str,
) -> Result<()> {
    let pid = pid.context("spawned core did not expose a process ID")?;
    let identity =
        process_identity(pid)?.with_context(|| format!("core process {pid} exited before runtime record {context}"))?;
    write_core_runtime_record(&CoreRuntimeRecord {
        pid,
        ipc_path: config.core_config.core_ipc_path.clone(),
        identity,
    })
    .await
    .with_context(|| format!("failed to write core runtime record {context}"))
}

impl CoreManager {
    pub(super) async fn start_watchdog(
        &self,
        child_guard: ChildGuard,
        config: ClashConfig,
        owner: OwnerIdentity,
        execution: Arc<crate::execution::CoreExecutionGuard>,
    ) {
        let running_pid_arc = Arc::clone(&self.running_pid);
        let start_time_arc = Arc::clone(&self.core_start_time);
        let started_at_arc = Arc::clone(&self.core_started_at);
        let last_exit_reason_arc = Arc::clone(&self.last_core_exit_reason);
        let restart_count_arc = Arc::clone(&self.restart_count);
        let last_recovery_at_arc = Arc::clone(&self.last_recovery_at);
        let failed_child_arc = Arc::clone(&self.failed_child);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let watchdog_config = watchdog_config();

        let handle = tokio::spawn(async move {
            let _execution = execution;
            let _dns_retirement = crate::core::tun_dns::RetirementGuard;
            let mut recovery_exhausted = false;
            let mut child_guard = Some(child_guard);
            let mut shutdown_rx = shutdown_rx;
            let mut restart_timestamps: Vec<Instant> = Vec::new();
            let mut consecutive_attempt = 0u32;

            'watchdog: loop {
                let Some(mut current_guard) = child_guard.take() else {
                    break;
                };

                let wait_result = {
                    let Some(child) = current_guard.inner() else {
                        break;
                    };

                    tokio::select! {
                        kill_by = &mut shutdown_rx => {
                            info!("Core watchdog received shutdown signal");
                            let grace = term_grace(kill_by.unwrap_or(None));
                            if let Err(error) = current_guard.terminate(grace).await {
                                *failed_child_arc.lock().await = Some(current_guard);
                                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                                return Err(error.context(
                                    "failed to terminate core during watchdog shutdown",
                                ));
                            }
                            break 'watchdog;
                        }
                        wait_result = child.wait() => wait_result,
                    }
                };

                let status = match wait_result {
                    Ok(status) => status,
                    Err(error) => {
                        warn!("Failed to wait for core process: {}", error);
                        recovery_exhausted = true;
                        break;
                    }
                };

                if let Err(error) = crate::core::tun_dns::apply(false).await {
                    warn!("Failed to release TUN DNS after core exit: {error:#}");
                }

                let uptime = start_time_arc.lock().await.map(|t| t.elapsed()).unwrap_or_default();
                let exit_reason = log_core_exit(&status, uptime);
                *last_exit_reason_arc.lock().await = Some(exit_reason);
                set_core_lifecycle_state(ServiceLifecycleState::RecoveringCore);

                // Dropping the guard cancels its readers.
                current_guard.drain_output().await;
                let _ = current_guard.take();
                running_pid_arc.store(0, Ordering::Release);
                started_at_arc.store(0, Ordering::Relaxed);
                remove_core_runtime_record().await;

                let now = Instant::now();
                restart_timestamps.retain(|t| now.duration_since(*t) < watchdog_config.restart_window);
                if restart_timestamps.is_empty() {
                    consecutive_attempt = 0;
                }
                restart_timestamps.push(now);

                loop {
                    if restart_timestamps.len() as u32 > watchdog_config.max_restarts {
                        error!(
                            "Core restarted {} times in {}s, giving up",
                            restart_timestamps.len(),
                            watchdog_config.restart_window.as_secs()
                        );
                        recovery_exhausted = true;
                        break 'watchdog;
                    }

                    let delay = backoff_delay(consecutive_attempt, watchdog_config.max_backoff);
                    info!(
                        "Restart attempt #{} after {}ms backoff",
                        consecutive_attempt + 1,
                        delay.as_millis()
                    );

                    if !delay.is_zero() {
                        tokio::select! {
                            _ = &mut shutdown_rx => break 'watchdog,
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }

                    if let Err(error) = prepare_core_ipc_socket(&config.core_config.core_ipc_path, &owner) {
                        error!("Failed to prepare core IPC before restart: {error:#}");
                        consecutive_attempt += 1;
                        let now = Instant::now();
                        restart_timestamps
                            .retain(|timestamp| now.duration_since(*timestamp) < watchdog_config.restart_window);
                        restart_timestamps.push(now);
                        continue;
                    }
                    let args = core_args(&config);
                    match run_with_logging(&config.core_config.core_path, &args, &config.log_config, &owner).await {
                        Ok(mut new_guard) => {
                            let new_pid = new_guard.id();
                            if let Err(error) = new_guard
                                .await_ipc(config.core_config.core_ipc_path.clone(), owner.clone())
                                .await
                            {
                                error!("Failed to secure restarted core IPC: {error:#}");
                                if let Err(kill_error) = new_guard.terminate(Duration::ZERO).await {
                                    error!("Failed to terminate core after IPC hardening failure: {kill_error:#}");
                                    let now_secs = unix_timestamp_secs();
                                    running_pid_arc.store(new_pid.unwrap_or_default(), Ordering::Release);
                                    *start_time_arc.lock().await = Some(Instant::now());
                                    started_at_arc.store(now_secs, Ordering::Relaxed);
                                    if let Err(record_error) = write_runtime_record_for_config(
                                        new_pid,
                                        &config,
                                        "after failed restart cleanup",
                                    )
                                    .await
                                    {
                                        warn!("Failed to record unconfirmed restarted core cleanup: {record_error:#}");
                                    }
                                    *failed_child_arc.lock().await = Some(new_guard);
                                    set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                                    return Err(kill_error
                                        .context("failed to terminate restarted core after IPC hardening failure"));
                                }
                                consecutive_attempt += 1;
                                let now = Instant::now();
                                restart_timestamps.retain(|timestamp| {
                                    now.duration_since(*timestamp) < watchdog_config.restart_window
                                });
                                restart_timestamps.push(now);
                                continue;
                            }
                            if let Err(record_error) =
                                write_runtime_record_for_config(new_pid, &config, "after restart").await
                            {
                                error!("Failed to commit restarted core runtime: {record_error:#}");
                                if let Err(kill_error) = new_guard.terminate(Duration::ZERO).await {
                                    let now_secs = unix_timestamp_secs();
                                    running_pid_arc.store(new_pid.unwrap_or_default(), Ordering::Release);
                                    *start_time_arc.lock().await = Some(Instant::now());
                                    started_at_arc.store(now_secs, Ordering::Relaxed);
                                    *failed_child_arc.lock().await = Some(new_guard);
                                    set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                                    return Err(anyhow!(
                                        "{record_error:#}; failed to terminate unrecorded restarted core: {kill_error:#}"
                                    ));
                                }
                                recovery_exhausted = true;
                                break 'watchdog;
                            }
                            if let Err(error) =
                                crate::core::tun_dns::apply_for_config(&config.core_config.config_path).await
                            {
                                error!("Failed to restore TUN DNS after core restart: {error:#}");
                                if let Err(kill_error) = new_guard.terminate(Duration::ZERO).await {
                                    running_pid_arc.store(new_pid.unwrap_or_default(), Ordering::Release);
                                    *failed_child_arc.lock().await = Some(new_guard);
                                    set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                                    return Err(
                                        kill_error.context("failed to retire core after TUN DNS recovery failure")
                                    );
                                }
                                recovery_exhausted = true;
                                break 'watchdog;
                            }
                            running_pid_arc.store(new_pid.unwrap_or_default(), Ordering::Release);
                            *start_time_arc.lock().await = Some(Instant::now());
                            let now_secs = unix_timestamp_secs();
                            started_at_arc.store(now_secs, Ordering::Relaxed);
                            restart_count_arc.fetch_add(1, Ordering::Relaxed);
                            last_recovery_at_arc.store(now_secs, Ordering::Relaxed);
                            consecutive_attempt += 1;
                            info!("Core restarted successfully (attempt #{})", consecutive_attempt);
                            set_core_lifecycle_state(ServiceLifecycleState::Running);
                            child_guard = Some(new_guard);
                            continue 'watchdog;
                        }
                        Err(error) => {
                            error!("Failed to restart core: {}", error);
                            consecutive_attempt += 1;
                            let now = Instant::now();
                            restart_timestamps.retain(|t| now.duration_since(*t) < watchdog_config.restart_window);
                            restart_timestamps.push(now);
                        }
                    }
                }
            }

            running_pid_arc.store(0, Ordering::Release);
            *start_time_arc.lock().await = None;
            started_at_arc.store(0, Ordering::Relaxed);
            remove_core_runtime_record().await;
            if recovery_exhausted {
                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
            }
            Ok(())
        });

        *self.watchdog_shutdown.lock().await = Some(shutdown_tx);
        *self.watchdog_handle.lock().await = Some(handle);
    }

    pub(super) async fn stop_watchdog(&self, kill_by: Option<Instant>) -> Result<()> {
        if let Some(shutdown_tx) = self.watchdog_shutdown.lock().await.take() {
            let _ = shutdown_tx.send(kill_by);
        }

        if let Some(handle) = self.watchdog_handle.lock().await.take() {
            handle.await.context("watchdog task failed to join")??;
            info!("Watchdog stopped");
        }

        Ok(())
    }
}
