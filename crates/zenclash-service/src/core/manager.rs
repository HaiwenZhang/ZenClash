// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::core::ClashConfig;
use crate::core::logger::{LOG_RING, set_or_update_writer, write_core_line};
use crate::core::process::process_identity;
use crate::core::reconcile::ensure_startup_reconciled;
use crate::core::runtime::{CoreRuntimeRecord, remove_core_runtime_record, write_core_runtime_record};
use crate::core::state::set_core_lifecycle_state;
use crate::core::structure::ServiceLifecycleState;
use crate::{OwnerIdentity, WriterConfig};
use anyhow::{Context as _, Result, anyhow};
use once_cell::sync::Lazy;
use std::process::Stdio;
#[cfg(feature = "test")]
use std::sync::Mutex as StdMutex;
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncBufReadExt;
use tokio::{io::BufReader, process::Command};
use tokio::{
    process::Child,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use tracing::{error, info, warn};

pub(super) const CORE_IPC_READY_TIMEOUT: Duration = Duration::from_secs(15);
const CORE_IPC_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Time the core gets after SIGTERM to shut down before SIGKILL.
pub(super) const CORE_TERM_GRACE: Duration = Duration::from_secs(3);
/// Bound on reading an exited core's remaining output.
pub(super) const CORE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Grace left before `kill_by`, evaluated when the core is actually stopped; `None` is the full grace.
fn term_grace(kill_by: Option<Instant>) -> Duration {
    kill_by.map_or(CORE_TERM_GRACE, |kill_by| {
        kill_by.saturating_duration_since(Instant::now()).min(CORE_TERM_GRACE)
    })
}

#[derive(Debug)]
pub struct CoreExitInfo {
    pub exit_code: Option<i32>,
    #[cfg(unix)]
    pub signal: Option<i32>,
    pub uptime: Duration,
}

impl CoreExitInfo {
    pub fn diagnosis(&self) -> &'static str {
        #[cfg(unix)]
        {
            if let Some(sig) = self.signal {
                return match sig {
                    9 => "Killed by OOM killer or admin (SIGKILL)",
                    11 => "Segmentation fault (SIGSEGV)",
                    15 => "Graceful shutdown (SIGTERM)",
                    6 => "Aborted (SIGABRT)",
                    _ => "Terminated by signal",
                };
            }
        }
        match self.exit_code {
            Some(0) => "Normal exit",
            Some(_) => "Abnormal exit",
            None => "Unknown exit reason",
        }
    }
}

pub struct ChildGuard {
    child: Option<Child>,
    readers: Vec<JoinHandle<()>>,
}

impl ChildGuard {
    fn inner(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    fn take(mut self) -> Option<Child> {
        self.child.take()
    }

    async fn await_ipc(&mut self, path: String, owner: OwnerIdentity) -> Result<()> {
        let child = self.inner().context("core process is unavailable during startup")?;
        let pid = child.id();
        tokio::select! {
            status = child.wait() => {
                let status = status.context("failed to wait for the starting core")?;
                self.drain_output().await;
                Err(anyhow!("core exited before its IPC endpoint became ready: {status}"))
            }
            result = secure_core_ipc_socket(path, owner, pid) => result,
        }
    }

    /// Stops the core and drains its output; a zero `grace` kills at once.
    async fn terminate(&mut self, grace: Duration) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            self.drain_output().await;
            info!("No running core process found");
            return Ok(());
        };
        let child_id = child.id();
        let result = stop_child(child, grace)
            .await
            .with_context(|| format!("failed to kill child {child_id:?}"));
        if result.is_err() {
            for reader in self.readers.drain(..) {
                reader.abort();
            }
        }
        result?;
        self.child.take();
        self.drain_output().await;
        info!("Core ({:?}) stopped", child_id);
        Ok(())
    }

    async fn drain_output(&mut self) {
        let deadline = tokio::time::Instant::now() + CORE_OUTPUT_DRAIN_TIMEOUT;
        // Awaited in place so a cancelled drain leaves the readers for `Drop` to abort.
        while let Some(reader) = self.readers.first_mut() {
            match tokio::time::timeout_at(deadline, &mut *reader).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!("Failed to finish reading exited core output: {error}"),
                Err(_) => {
                    warn!("Core output stayed open after the core exited; dropping the rest");
                    reader.abort();
                }
            }
            self.readers.remove(0);
        }
    }
}

/// SIGTERM, then SIGKILL after `grace`; Windows has no SIGTERM.
async fn stop_child(child: &mut Child, grace: Duration) -> std::io::Result<()> {
    #[cfg(not(unix))]
    let _ = grace;
    // `id()` is `None` once reaped, so the PID cannot have been reused.
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok())
        && !grace.is_zero()
    {
        if unsafe { platform_lib::kill(pid, platform_lib::SIGTERM) } == 0 {
            info!("Sent SIGTERM to core ({pid})");
            match tokio::time::timeout(grace, child.wait()).await {
                Ok(Ok(status)) => {
                    info!("Core ({pid}) exited after SIGTERM: {status}");
                    return Ok(());
                }
                Ok(Err(error)) => warn!("Failed to wait for core ({pid}) after SIGTERM: {error}"),
                Err(_) => warn!("Core ({pid}) did not exit within {grace:?} of SIGTERM, sending SIGKILL"),
            }
        } else {
            warn!(
                "Failed to send SIGTERM to core ({pid}): {}",
                std::io::Error::last_os_error()
            );
        }
    }
    child.kill().await
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        for reader in self.readers.drain(..) {
            reader.abort();
        }
        if let Some(mut child) = self.child.take() {
            tokio::spawn(async move {
                if let Err(e) = child.kill().await {
                    warn!("Failed to kill child ({:?}): {e}", child.id());
                } else {
                    info!("Successfully killed child ({:?})", child.id());
                }
            });
        } else {
            info!("No running core process found");
        }
    }
}

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
static WATCHDOG_CONFIG_OVERRIDE: Lazy<StdMutex<Option<WatchdogConfig>>> = Lazy::new(|| StdMutex::new(None));

#[cfg(feature = "test")]
/// Sets the isolated test watchdog override.
///
/// # Panics
/// Panics if a prior panic poisoned the test-only watchdog configuration mutex.
pub fn set_core_watchdog_config_for_tests(config: Option<CoreWatchdogTestConfig>) {
    let mut guard = WATCHDOG_CONFIG_OVERRIDE.lock().unwrap();
    *guard = config.map(|config| WatchdogConfig {
        max_restarts: config.max_restarts,
        restart_window: config.restart_window,
        max_backoff: config.max_backoff,
    });
}

fn watchdog_config() -> WatchdogConfig {
    #[cfg(feature = "test")]
    if let Some(config) = *WATCHDOG_CONFIG_OVERRIDE.lock().unwrap() {
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

fn core_args(config: &ClashConfig) -> Vec<String> {
    vec![
        "-d".to_string(),
        config.core_config.config_dir.clone(),
        "-f".to_string(),
        config.core_config.config_path.clone(),
        if cfg!(windows) {
            "-ext-ctl-pipe".to_string()
        } else {
            "-ext-ctl-unix".to_string()
        },
        config.core_config.core_ipc_path.clone(),
    ]
}

fn log_core_exit(status: &std::process::ExitStatus, uptime: Duration) -> String {
    let exit_info = CoreExitInfo {
        exit_code: status.code(),
        #[cfg(unix)]
        signal: {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        },
        uptime,
    };

    error!(
        "Core exited unexpectedly - code: {:?}, diagnosis: {}, uptime: {:.1}s",
        exit_info.exit_code,
        exit_info.diagnosis(),
        exit_info.uptime.as_secs_f64()
    );

    #[cfg(unix)]
    if let Some(sig) = exit_info.signal {
        error!("Core terminated by signal: {}", sig);
    }

    format!("{} (code: {:?})", exit_info.diagnosis(), exit_info.exit_code)
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn non_zero_u32(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

fn non_zero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

async fn write_runtime_record_for_config(pid: Option<u32>, config: &ClashConfig, context: &'static str) -> Result<()> {
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

pub struct CoreManager {
    running_pid: Arc<AtomicU32>,
    running_config: Mutex<Option<ClashConfig>>,
    core_start_time: Arc<Mutex<Option<Instant>>>,
    core_started_at: Arc<AtomicU64>,
    last_core_exit_reason: Arc<Mutex<Option<String>>>,
    restart_count: Arc<AtomicU32>,
    last_recovery_at: Arc<AtomicU64>,
    watchdog_shutdown: Mutex<Option<oneshot::Sender<Option<Instant>>>>,
    watchdog_handle: Mutex<Option<JoinHandle<Result<()>>>>,
    failed_child: Arc<Mutex<Option<ChildGuard>>>,
    execution: Mutex<Option<std::sync::Arc<crate::execution::CoreExecutionGuard>>>,
}

#[derive(Debug, Clone)]
pub(super) struct CoreStatusSnapshot {
    pub(super) core_pid: Option<u32>,
    pub(super) core_started_at: Option<u64>,
    pub(super) last_core_exit_reason: Option<String>,
    pub(super) restart_count: u32,
    pub(super) last_recovery_at: Option<u64>,
}

impl CoreManager {
    fn new() -> Self {
        CoreManager {
            running_pid: Arc::new(AtomicU32::new(0)),
            running_config: Mutex::new(None),
            core_start_time: Arc::new(Mutex::new(None)),
            core_started_at: Arc::new(AtomicU64::new(0)),
            last_core_exit_reason: Arc::new(Mutex::new(None)),
            restart_count: Arc::new(AtomicU32::new(0)),
            last_recovery_at: Arc::new(AtomicU64::new(0)),
            watchdog_shutdown: Mutex::new(None),
            watchdog_handle: Mutex::new(None),
            failed_child: Arc::new(Mutex::new(None)),
            execution: Mutex::new(None),
        }
    }

    /// Returns the live core's PID and configuration from one consistent read.
    /// The configuration guard must be acquired before reading the PID because the watchdog can
    /// clear it without holding that guard.
    pub(super) async fn running_core_config(&self) -> Option<(u32, ClashConfig)> {
        let config = self.running_config.lock().await;
        let pid = self.running_pid.load(Ordering::Acquire);
        if pid == 0 {
            return None;
        }
        config.clone().map(|config| (pid, config))
    }

    pub async fn start_core(&self, config: ClashConfig, owner: OwnerIdentity) -> Result<()> {
        ensure_startup_reconciled()?;
        set_core_lifecycle_state(ServiceLifecycleState::Starting);
        if self.running_pid.load(Ordering::Relaxed) != 0 || self.execution.lock().await.is_some() {
            info!("Core is already running, stopping existing instance");
            self.stop_core().await?;
        }

        let execution = std::sync::Arc::new(crate::execution::CoreExecutionGuard::acquire()?);
        let config_path = config.core_config.config_path.clone();
        let result = self.start_reserved_core(config, owner, execution.clone()).await;
        if result.is_ok() || self.running_pid.load(Ordering::Acquire) != 0 {
            *self.execution.lock().await = Some(execution);
        }
        result?;
        // CRITICAL REGRESSION: start/recovery cannot rely on a GUI readback to
        // move macOS LAN DNS into TUN. A live core and route can still fail all
        // domain-based requests without this lease; see core/tun_dns.rs.
        if let Err(error) = super::tun_dns::apply_for_config(&config_path).await {
            self.stop_core()
                .await
                .context("failed to retire core after TUN DNS setup failure")?;
            return Err(error);
        }
        Ok(())
    }

    async fn start_reserved_core(
        &self,
        config: ClashConfig,
        owner: OwnerIdentity,
        execution: Arc<crate::execution::CoreExecutionGuard>,
    ) -> Result<()> {
        info!("Starting core with config: {:?}", config);

        prepare_core_ipc_socket(&config.core_config.core_ipc_path, &owner)?;
        let args = core_args(&config);

        let mut child_guard =
            run_with_logging(&config.core_config.core_path, &args, &config.log_config, &owner).await?;
        let child_pid = child_guard.id();

        if let Err(error) = child_guard
            .await_ipc(config.core_config.core_ipc_path.clone(), owner.clone())
            .await
        {
            // Never became ready: kill at once.
            if let Err(kill_error) = child_guard.terminate(Duration::ZERO).await {
                let now_secs = unix_timestamp_secs();
                self.running_pid.store(child_pid.unwrap_or_default(), Ordering::Release);
                *self.running_config.lock().await = Some(config.clone());
                *self.core_start_time.lock().await = Some(Instant::now());
                self.core_started_at.store(now_secs, Ordering::Relaxed);
                if let Err(record_error) =
                    write_runtime_record_for_config(child_pid, &config, "after failed initial cleanup").await
                {
                    warn!("Failed to record unconfirmed core cleanup: {record_error:#}");
                }
                *self.failed_child.lock().await = Some(child_guard);
                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                return Err(anyhow!(
                    "failed to secure core IPC: {error:#}; failed to terminate spawned core: {kill_error:#}"
                ));
            }
            return Err(error);
        }

        if let Err(record_error) = write_runtime_record_for_config(child_pid, &config, "after start").await {
            if let Err(kill_error) = child_guard.terminate(Duration::ZERO).await {
                let now_secs = unix_timestamp_secs();
                self.running_pid.store(child_pid.unwrap_or_default(), Ordering::Release);
                *self.running_config.lock().await = Some(config.clone());
                *self.core_start_time.lock().await = Some(Instant::now());
                self.core_started_at.store(now_secs, Ordering::Relaxed);
                *self.failed_child.lock().await = Some(child_guard);
                set_core_lifecycle_state(ServiceLifecycleState::Fatal);
                return Err(anyhow!(
                    "{record_error:#}; failed to terminate unrecorded core: {kill_error:#}"
                ));
            }
            return Err(record_error);
        }

        *self.core_start_time.lock().await = Some(Instant::now());
        self.core_started_at.store(unix_timestamp_secs(), Ordering::Relaxed);
        self.running_pid.store(child_pid.unwrap_or_default(), Ordering::Release);
        *self.running_config.lock().await = Some(config.clone());

        self.start_watchdog(child_guard, config, owner, execution).await;
        set_core_lifecycle_state(ServiceLifecycleState::Running);

        Ok(())
    }

    pub async fn stop_core(&self) -> Result<()> {
        self.stop_core_by(None).await
    }

    /// Stops the core; IPC handlers pass the deadline by which it must be SIGKILLed.
    pub(super) async fn stop_core_by(&self, kill_by: Option<Instant>) -> Result<()> {
        info!("Stopping core");
        LOG_RING.clear_logs();

        // Restore the system resolver while the TUN routes still exist. The
        // watchdog also releases it on an unexpected exit before any restart.
        // Removing this cleanup can leave DNS pointing at the retired TUN path.
        super::tun_dns::apply(false).await?;

        let watchdog_result = self.stop_watchdog(kill_by).await;
        let mut recovered_failed_child = false;
        // Bound first: an `if let` scrutinee would keep the lock through the block.
        let failed_child = self.failed_child.lock().await.take();
        if let Some(mut child_guard) = failed_child {
            if let Err(error) = child_guard.terminate(term_grace(kill_by)).await {
                *self.failed_child.lock().await = Some(child_guard);
                return Err(error.context("failed to retry termination of tracked core"));
            }
            recovered_failed_child = true;
        }
        if !recovered_failed_child {
            watchdog_result?;
        }

        self.running_pid.store(0, Ordering::Release);
        *self.core_start_time.lock().await = None;
        self.core_started_at.store(0, Ordering::Relaxed);

        let start_clash = self.running_config.lock().await.take();
        let core_ipc_path = start_clash
            .as_ref()
            .map(|config| config.core_config.core_ipc_path.clone());
        if let Some(config) = start_clash {
            info!("Clearing running config: {:?}", config);
        } else {
            info!("No running config to clear");
        }

        remove_core_runtime_record().await;
        self.after_stop(core_ipc_path).await;
        set_core_lifecycle_state(ServiceLifecycleState::Running);

        Ok(())
    }

    async fn start_watchdog(
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
            let _dns_retirement = super::tun_dns::RetirementGuard;
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

                if let Err(error) = super::tun_dns::apply(false).await {
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
                            if let Err(error) = super::tun_dns::apply_for_config(&config.core_config.config_path).await
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

    async fn stop_watchdog(&self, kill_by: Option<Instant>) -> Result<()> {
        if let Some(shutdown_tx) = self.watchdog_shutdown.lock().await.take() {
            let _ = shutdown_tx.send(kill_by);
        }

        if let Some(handle) = self.watchdog_handle.lock().await.take() {
            handle.await.context("watchdog task failed to join")??;
            info!("Watchdog stopped");
        }

        Ok(())
    }

    pub(super) async fn status(&self) -> CoreStatusSnapshot {
        CoreStatusSnapshot {
            core_pid: non_zero_u32(self.running_pid.load(Ordering::Relaxed)),
            core_started_at: non_zero_u64(self.core_started_at.load(Ordering::Relaxed)),
            last_core_exit_reason: self.last_core_exit_reason.lock().await.clone(),
            restart_count: self.restart_count.load(Ordering::Relaxed),
            last_recovery_at: non_zero_u64(self.last_recovery_at.load(Ordering::Relaxed)),
        }
    }

    async fn after_stop(&self, core_ipc_path: Option<String>) {
        self.execution.lock().await.take();
        #[cfg(unix)]
        {
            use std::path::Path;
            use tokio::fs;

            if let Some(core_ipc_path) = core_ipc_path {
                let target = Path::new(&core_ipc_path);
                info!("Removing socket file {:?}", target);
                if !target.exists() {
                    info!("{:?} does not exist, no need to remove", target);
                } else {
                    match fs::remove_file(target).await {
                        Ok(_) => info!("Successfully removed {:?}", target),
                        Err(e) => warn!("Failed to remove {:?}: {}", target, e),
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = core_ipc_path;
        }
        LOG_RING.clear_logs();
    }
}

pub async fn run_with_logging(
    bin_path: &str,
    args: &[String],
    writer_config: &WriterConfig,
    owner: &OwnerIdentity,
) -> Result<ChildGuard> {
    // Every spawn is checked here rather than at the IPC boundary alone: desired-state
    // restoration and watchdog restarts reach this point without passing through
    // `prepare_runtime`, and state written by an older build can name a path this build refuses.
    let core = crate::core::runtime_generation::validate_core_path(bin_path)?;
    let bin_path = core.executable();
    set_or_update_writer(writer_config)?;

    #[cfg(windows)]
    let child = {
        let OwnerIdentity::Windows { sid } = owner else {
            return Err(anyhow!("Windows core requires a Windows owner identity"));
        };
        Command::new(bin_path)
            .args(args)
            .env("LISTEN_NAMEDPIPE_SDDL", windows_owner_pipe_sddl(sid))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
    };

    #[cfg(unix)]
    let child = unsafe {
        let _ = owner;
        Command::new(bin_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .pre_exec(|| {
                platform_lib::umask(0o007);
                Ok(())
            })
            .spawn()?
    };

    let mut child_guard = ChildGuard {
        child: Some(child),
        readers: Vec::new(),
    };

    let (Some(stdout), Some(stderr)) = (
        child_guard.inner().and_then(|c| c.stdout.take()),
        child_guard.inner().and_then(|c| c.stderr.take()),
    ) else {
        return Err(anyhow!("Failed to capture child output"));
    };

    child_guard.readers.push(spawn_line_reader(stdout, log::Level::Info));
    child_guard.readers.push(spawn_line_reader(stderr, log::Level::Error));

    Ok(child_guard)
}

fn spawn_line_reader<R>(reader: R, level: log::Level) -> JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            write_core_line(level, &line);
            LOG_RING.append_log(line);
        }
    })
}

fn prepare_core_ipc_socket(core_ipc_path: &str, owner: &OwnerIdentity) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let OwnerIdentity::Unix { uid, .. } = owner else {
            anyhow::bail!("Unix core IPC path received a non-Unix owner");
        };
        let target = std::path::Path::new(core_ipc_path);
        let directory = target.parent().context("core IPC path has no parent directory")?;
        let directory_c = std::ffi::CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| anyhow::anyhow!("core IPC directory contains NUL"))?;
        let fd = unsafe {
            platform_lib::open(
                directory_c.as_ptr(),
                platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to open core IPC directory {directory:?}"));
        }

        let result = (|| -> Result<()> {
            let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
            if unsafe { platform_lib::fstat(fd, &mut stat) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to inspect core IPC directory");
            }
            let effective_uid = unsafe { platform_lib::geteuid() };
            if stat.st_mode & platform_lib::S_IFMT != platform_lib::S_IFDIR
                || (stat.st_uid != 0 && stat.st_uid != *uid && stat.st_uid != effective_uid)
            {
                anyhow::bail!("core IPC directory has an unexpected owner or file type");
            }
            if unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to make core IPC directory private");
            }
            if effective_uid == 0 && unsafe { platform_lib::fchown(fd, 0, 0) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to take ownership of core IPC directory");
            }

            let file_name = target.file_name().context("core IPC path has no file name")?;
            let file_name_c = std::ffi::CString::new(file_name.as_bytes())
                .map_err(|_| anyhow::anyhow!("core IPC file name contains NUL"))?;
            if unsafe { platform_lib::unlinkat(fd, file_name_c.as_ptr(), 0) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error).context("failed to clear stale core IPC entry");
                }
            }
            Ok(())
        })();
        unsafe { platform_lib::close(fd) };
        result
    }

    #[cfg(windows)]
    {
        let _ = (core_ipc_path, owner);
        Ok(())
    }
}

#[cfg(unix)]
fn grant_core_ipc_directory_to_owner(target: &std::path::Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let directory = target.parent().context("core IPC path has no parent directory")?;
    let directory_c = std::ffi::CString::new(directory.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("core IPC directory contains NUL"))?;
    let fd = unsafe {
        platform_lib::open(
            directory_c.as_ptr(),
            platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to reopen core IPC directory");
    }
    let result = if unsafe { platform_lib::geteuid() } == 0 && unsafe { platform_lib::fchown(fd, uid, gid) } != 0 {
        Err(std::io::Error::last_os_error()).context("failed to grant core IPC directory to owner")
    } else if unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } != 0 {
        Err(std::io::Error::last_os_error()).context("failed to secure granted core IPC directory")
    } else {
        Ok(())
    };
    unsafe { platform_lib::close(fd) };
    result
}

async fn secure_core_ipc_socket(core_ipc_path: String, owner: OwnerIdentity, expected_pid: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::FileTypeExt as _;

        let _ = expected_pid;
        let OwnerIdentity::Unix { uid, gid } = owner else {
            anyhow::bail!("Unix core IPC path received a non-Unix owner");
        };
        let target = std::path::PathBuf::from(core_ipc_path);
        let mut found = false;
        let deadline = tokio::time::Instant::now() + CORE_IPC_READY_TIMEOUT;
        loop {
            match tokio::fs::symlink_metadata(&target).await {
                Ok(metadata) if metadata.file_type().is_socket() => {
                    found = true;
                    break;
                }
                Ok(_) => {
                    anyhow::bail!("core IPC path {target:?} is not a socket");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(CORE_IPC_POLL_INTERVAL).await;
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }
        if !found {
            anyhow::bail!("core IPC socket did not appear at {target:?} within {CORE_IPC_READY_TIMEOUT:?}");
        }
        let path = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| anyhow::anyhow!("core IPC socket path contains NUL"))?;
        let chown_ok =
            unsafe { platform_lib::geteuid() } != 0 || unsafe { platform_lib::lchown(path.as_ptr(), uid, gid) } == 0;
        let chmod_ok = unsafe {
            platform_lib::fchmodat(
                platform_lib::AT_FDCWD,
                path.as_ptr(),
                0o600 as platform_lib::mode_t,
                platform_lib::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        let os_error = (!chown_ok || !chmod_ok).then(std::io::Error::last_os_error);
        if !chown_ok || !chmod_ok {
            return Err(os_error.unwrap_or_else(std::io::Error::last_os_error).into());
        }
        grant_core_ipc_directory_to_owner(&target, uid, gid)?;
        info!("Secured core IPC socket {:?} for uid {}", target, uid);
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_KERNEL_OBJECT, SetSecurityInfo,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        };
        use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

        let OwnerIdentity::Windows { sid } = owner else {
            anyhow::bail!("Windows core IPC path received a non-Windows owner");
        };
        let mut pipe: Vec<u16> = std::ffi::OsStr::new(&core_ipc_path).encode_wide().collect();
        pipe.push(0);
        let deadline = tokio::time::Instant::now() + CORE_IPC_READY_TIMEOUT;
        let handle_value = loop {
            let handle_value = unsafe {
                CreateFileW(
                    pipe.as_ptr(),
                    READ_CONTROL | WRITE_DAC,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            } as isize;
            if handle_value != INVALID_HANDLE_VALUE as isize {
                break handle_value;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("core IPC pipe did not become ready within {CORE_IPC_READY_TIMEOUT:?}"));
            }
            tokio::time::sleep(CORE_IPC_POLL_INTERVAL).await;
        };
        let handle = handle_value as *mut std::ffi::c_void;
        let _pipe = unsafe { std::fs::File::from_raw_handle(handle) };
        let mut server_pid = 0u32;
        if unsafe { GetNamedPipeServerProcessId(handle_value as _, &mut server_pid) } == 0 {
            return Err(std::io::Error::last_os_error()).context("failed to identify core IPC pipe server");
        }
        if Some(server_pid) != expected_pid {
            anyhow::bail!("core IPC pipe server PID {server_pid} did not match spawned core PID {expected_pid:?}");
        }

        let sddl = windows_owner_pipe_sddl(&sid);
        let mut wide: Vec<u16> = sddl.encode_utf16().collect();
        wide.push(0);
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(std::io::Error::last_os_error().into());
        }
        struct LocalDescriptor(*mut std::ffi::c_void);
        impl Drop for LocalDescriptor {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let descriptor_guard = LocalDescriptor(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(descriptor_guard.0, &mut present, &mut dacl, &mut defaulted) } == 0
            || present == 0
            || dacl.is_null()
        {
            anyhow::bail!("failed to read owner core IPC DACL");
        }
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            anyhow::bail!("failed to secure core IPC pipe: Windows error {status}");
        }
        info!("Secured core IPC pipe for owner SID");
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn windows_owner_pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)")
}

pub static CORE_MANAGER: Lazy<Arc<Mutex<CoreManager>>> = Lazy::new(|| Arc::new(Mutex::new(CoreManager::new())));

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_drains_final_output_but_bounds_inherited_open_streams() {
        let (written, final_output) = oneshot::channel();
        let reader = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = written.send("last core line");
        });
        let stuck = tokio::spawn(std::future::pending::<()>());
        let stuck_abort = stuck.abort_handle();
        let another_stuck = tokio::spawn(std::future::pending::<()>());
        let mut guard = ChildGuard {
            child: None,
            readers: vec![reader, stuck, another_stuck],
        };
        let started = Instant::now();
        guard.terminate(Duration::ZERO).await.unwrap();
        assert_eq!(final_output.await.unwrap(), "last core line");
        assert!(guard.readers.is_empty());
        assert!(started.elapsed() < CORE_OUTPUT_DRAIN_TIMEOUT + Duration::from_secs(1));
        tokio::task::yield_now().await;
        assert!(stuck_abort.is_finished());
    }

    #[test]
    fn queued_stop_never_gets_fresh_grace_after_its_deadline() {
        assert_eq!(
            term_grace(Some(Instant::now() - Duration::from_secs(1))),
            Duration::ZERO
        );
        assert_eq!(
            term_grace(Some(Instant::now() + Duration::from_secs(30))),
            CORE_TERM_GRACE
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{prepare_core_ipc_socket, secure_core_ipc_socket};
    use crate::OwnerIdentity;
    use serial_test::serial;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    #[tokio::test]
    #[serial]
    async fn owner_core_socket_is_private() -> anyhow::Result<()> {
        let directory = std::env::temp_dir().join(format!("cvs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory)?;
        let path = directory.join("mihomo.sock");
        let listener = tokio::net::UnixListener::bind(&path)?;
        let owner = OwnerIdentity::Unix {
            uid: unsafe { platform_lib::geteuid() },
            gid: unsafe { platform_lib::getegid() },
        };

        prepare_core_ipc_socket(&path.to_string_lossy(), &owner)?;
        drop(listener);
        let listener = tokio::net::UnixListener::bind(&path)?;
        secure_core_ipc_socket(path.to_string_lossy().into_owned(), owner, None).await?;

        let mut mode = 0;
        for _ in 0..40 {
            mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
            if mode == 0o600 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(mode, 0o600);
        drop(listener);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}

#[cfg(test)]
mod windows_pipe_tests {
    use super::windows_owner_pipe_sddl;

    #[test]
    fn windows_owner_pipe_dacl_excludes_everyone_and_authenticated_users() {
        let sddl = windows_owner_pipe_sddl("S-1-5-21-1-2-3-1001");

        assert!(sddl.contains(";;;S-1-5-21-1-2-3-1001)"));
        assert!(sddl.contains(";;;SY)"));
        assert!(sddl.contains(";;;BA)"));
        assert!(!sddl.contains(";;;WD)"));
        assert!(!sddl.contains(";;;AU)"));
    }
}
