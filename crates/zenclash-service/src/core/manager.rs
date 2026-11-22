#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Core process lifecycle coordinator: start, stop, status reporting, and watchdog delegation.

mod ipc_security;
mod process_guard;
mod watchdog;

#[cfg(test)]
mod tests;

pub(super) use ipc_security::CORE_IPC_READY_TIMEOUT;
#[cfg(feature = "test")]
pub use watchdog::{CoreWatchdogTestConfig, set_core_watchdog_config_for_tests};

use self::ipc_security::prepare_core_ipc_socket;
use self::process_guard::{ChildGuard, core_args, run_with_logging, term_grace};
use self::watchdog::{unix_timestamp_secs, write_runtime_record_for_config};
use crate::OwnerIdentity;
use crate::core::ClashConfig;
use crate::core::logger::LOG_RING;
use crate::core::reconcile::ensure_startup_reconciled;
use crate::core::runtime::remove_core_runtime_record;
use crate::core::state::set_core_lifecycle_state;
use crate::core::structure::ServiceLifecycleState;
use anyhow::{Context as _, Result, anyhow};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Time the core gets after SIGTERM to shut down before SIGKILL.
pub(super) const CORE_TERM_GRACE: Duration = Duration::from_secs(3);
/// Bound on reading an exited core's remaining output.
pub(super) const CORE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

fn non_zero_u32(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

fn non_zero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
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
    execution: Mutex<Option<Arc<crate::execution::CoreExecutionGuard>>>,
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
        Self {
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

        let execution = Arc::new(crate::execution::CoreExecutionGuard::acquire()?);
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

pub static CORE_MANAGER: LazyLock<Arc<Mutex<CoreManager>>> = LazyLock::new(|| Arc::new(Mutex::new(CoreManager::new())));
