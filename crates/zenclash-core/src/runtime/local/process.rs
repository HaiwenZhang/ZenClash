use std::{
    path::PathBuf,
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[cfg(test)]
use std::{sync::atomic::AtomicBool, time::Duration};

use parking_lot::{Mutex, RwLock};

use crate::{
    CoreCapabilities, CoreConfigValidator, CoreKind, MihomoEndpoint, MihomoError, MihomoResult,
    runtime::launcher::{CoreLaunchDescriptor, CoreProcessLease},
};

mod input;
mod launch_config;
mod launch_logs;
use launch_logs::LaunchLogHistory;
mod mihomo_resources;
mod resources;
mod restart;
#[cfg(test)]
use restart::readiness_attempt_timeout;
mod spawn;
#[cfg(test)]
use spawn::{collect_output, stop_running_child};
use spawn::{local_launch_payload, spawn_child, stop_child};

#[cfg(test)]
mod tests;

pub use launch_config::CoreLaunchConfig;
pub use mihomo_resources::MihomoRuntimeResources;
pub(crate) use resources::service_core_source;
pub use resources::{bundled_default_profile, bundled_recovery_profile};

const MAX_LOG_LINES: usize = 1_000;

enum ExecutionReservation {
    Native(crate::runtime::service::ipc::CoreExecutionGuard),
    #[cfg(test)]
    Test(TestExecutionGuard),
}

impl ExecutionReservation {
    fn release_after_exit(self, confirm: impl FnMut() -> bool + Send + 'static) {
        match self {
            Self::Native(guard) => guard.release_after_exit(confirm),
            #[cfg(test)]
            Self::Test(_guard) => {}
        }
    }
}

#[cfg(test)]
struct TestExecutionGuard {
    held: Arc<AtomicBool>,
}

#[cfg(test)]
impl Drop for TestExecutionGuard {
    fn drop(&mut self) {
        self.held.store(false, Ordering::Release);
    }
}

/// Owns a managed Mihomo child, execution reservation, and bounded output history.
pub struct ManagedCoreProcess {
    child: Mutex<Option<Child>>,
    started_at_secs: AtomicU64,
    execution: Mutex<Option<ExecutionReservation>>,
    isolated_test_child: bool,
    logs: Arc<RwLock<LaunchLogHistory>>,
    last_exit_reason: RwLock<Option<String>>,
    config: CoreLaunchConfig,
    recovery_asset_root: RwLock<Option<PathBuf>>,
    #[cfg(test)]
    drop_gate: Mutex<Option<TestDropGate>>,
    #[cfg(test)]
    test_execution_slot: Option<Arc<AtomicBool>>,
}

#[cfg(test)]
struct TestDropGate {
    started: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
pub(crate) struct TestDropPause {
    started: std::sync::mpsc::Receiver<()>,
    resume: Option<std::sync::mpsc::Sender<()>>,
}

#[cfg(test)]
impl TestDropPause {
    pub(crate) fn wait_started(&self) {
        self.started.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    pub(crate) fn resume(&mut self) {
        if let Some(resume) = self.resume.take() {
            let _ = resume.send(());
        }
    }
}

#[cfg(test)]
impl Drop for TestDropPause {
    fn drop(&mut self) {
        self.resume();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Point-in-time state of a locally managed core child.
pub struct CoreProcessSnapshot {
    /// Runtime core implementation represented by this process.
    pub kind: CoreKind,
    /// Whether the child is still running.
    pub running: bool,
    /// Live process identifier, absent after exit or stop.
    pub pid: Option<u32>,
    /// Unix timestamp of the current live child's successful spawn.
    pub started_at_secs: Option<u64>,
    /// Last unexpected child exit status observed through the owned handle.
    pub exit_reason: Option<String>,
    /// Executable used to launch the child.
    pub binary: PathBuf,
    /// Original startup source; Local Mihomo receives a frozen copy through stdin.
    pub config_file: PathBuf,
    /// Writable data directory used by the selected managed core.
    pub home_dir: PathBuf,
    /// Most recent bounded stdout and stderr lines.
    pub logs: Vec<String>,
}

impl ManagedCoreProcess {
    /// Creates an owned, stopped runtime without filesystem or process operations.
    ///
    /// Recovery callers publish this owner before invoking the authorized restart
    /// path, so shutdown can still reach a child whose readiness fails or is cancelled.
    /// This performs no executable or configuration validation and grants no startup authority.
    #[must_use]
    pub fn prepare_stopped(config: CoreLaunchConfig) -> Arc<Self> {
        Arc::new(Self {
            child: Mutex::new(None),
            started_at_secs: AtomicU64::new(0),
            execution: Mutex::new(None),
            isolated_test_child: false,
            logs: Arc::new(RwLock::new(LaunchLogHistory::default())),
            last_exit_reason: RwLock::new(None),
            config,
            recovery_asset_root: RwLock::new(None),
            #[cfg(test)]
            drop_gate: Mutex::new(None),
            #[cfg(test)]
            test_execution_slot: None,
        })
    }

    /// Starts the selected managed core and attaches bounded output collectors.
    ///
    /// # Errors
    ///
    /// Returns an error when the data directory, child process, or collector
    /// threads cannot be created. A partially started child is terminated.
    pub fn spawn(config: CoreLaunchConfig) -> MihomoResult<Arc<Self>> {
        Self::spawn_inner(config, false)
    }

    /// Starts a synthetic UI test child outside the production kernel slot.
    /// Never used by application startup, service handover or real-core acceptance.
    ///
    /// # Errors
    /// Reports the same child/configuration failures as ordinary spawning.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn spawn_isolated_for_test(config: CoreLaunchConfig) -> MihomoResult<Arc<Self>> {
        Self::spawn_inner(config, true)
    }

    fn spawn_inner(config: CoreLaunchConfig, isolated_test_child: bool) -> MihomoResult<Arc<Self>> {
        let _write_lease = crate::platform::data_coordinator::DataWriteLease::shared(
            launch_write_scopes(&config),
        )?;
        let payload = local_launch_payload(&config)?;
        let input = payload
            .as_deref()
            .map(|payload| input::snapshot(&config.home_dir, payload))
            .transpose()?;
        let logs = Arc::new(RwLock::new(LaunchLogHistory::default()));
        let execution = reserve_native_execution(config.kind, isolated_test_child)?;
        let child = spawn_child(&config, logs.clone(), input, None)?;

        Ok(Arc::new(Self {
            child: Mutex::new(Some(child)),
            started_at_secs: AtomicU64::new(unix_timestamp_secs()),
            execution: Mutex::new(execution),
            isolated_test_child,
            logs,
            last_exit_reason: RwLock::new(None),
            config,
            recovery_asset_root: RwLock::new(None),
            #[cfg(test)]
            drop_gate: Mutex::new(None),
            #[cfg(test)]
            test_execution_slot: None,
        }))
    }

    #[cfg(test)]
    pub(crate) fn pause_last_drop_for_test(&self) -> TestDropPause {
        let (started, observed) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        let previous = self.drop_gate.lock().replace(TestDropGate {
            started,
            resume: resumed,
        });
        assert!(previous.is_none());
        TestDropPause {
            started: observed,
            resume: Some(resume),
        }
    }

    /// Returns the controller endpoint assigned to this process.
    #[must_use]
    pub const fn endpoint(&self) -> &MihomoEndpoint {
        &self.config.endpoint
    }

    pub(crate) fn native_controller(
        &self,
    ) -> MihomoResult<Option<crate::runtime::service::ipc::NativeController>> {
        let Some(path) = self.config.endpoint.ipc_path() else {
            return Ok(None);
        };
        let pid = self
            .child
            .lock()
            .as_ref()
            .map(Child::id)
            .ok_or_else(|| MihomoError::Process("Local core is stopped".into()))?;
        Ok(Some(
            crate::runtime::service::ipc::NativeController::new_local(
                path.to_path_buf(),
                pid,
                self.config.endpoint.secret.clone(),
            ),
        ))
    }

    /// Returns the concrete runtime core owned by this process.
    #[must_use]
    pub const fn kind(&self) -> CoreKind {
        self.config.kind
    }

    /// Returns the feature contract for the concrete runtime core.
    #[must_use]
    pub const fn capabilities(&self) -> CoreCapabilities {
        self.config.capabilities()
    }

    /// Returns a target-core validator bound to this managed process.
    #[must_use]
    pub fn config_validator(&self) -> CoreConfigValidator {
        CoreConfigValidator::new(
            self.config.kind,
            self.config.binary.clone(),
            self.config.home_dir.clone(),
        )
        .with_recovery_asset_root(self.recovery_asset_root.read().clone())
    }

    pub(crate) fn write_scopes(&self) -> Vec<PathBuf> {
        let mut scopes = launch_write_scopes(&self.config);
        scopes.extend(self.recovery_asset_root.read().clone());
        scopes
    }

    pub(crate) fn set_recovery_asset_root(&self, root: Option<PathBuf>) {
        *self.recovery_asset_root.write() = root;
    }

    /// Returns immutable launch metadata without waiting for process transitions.
    #[must_use]
    pub const fn launch_config(&self) -> &CoreLaunchConfig {
        &self.config
    }

    /// Copies bounded output history without acquiring the process lifecycle lock.
    #[must_use]
    pub fn recent_logs(&self) -> Vec<String> {
        self.logs.read().recent_messages()
    }

    /// Checks whether the child has not exited yet.
    /// Run off the UI thread: a concurrent stop or restart can hold the child lock.
    #[must_use]
    pub fn is_running(&self) -> bool {
        let mut child_slot = self.child.lock();
        let Some(child) = child_slot.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                *self.last_exit_reason.write() = Some(format!("内核退出状态：{status}"));
                self.started_at_secs.store(0, Ordering::Relaxed);
                child_slot.take();
                self.execution.lock().take();
                false
            }
            Err(error) => {
                tracing::warn!(%error, "failed to query managed core process status");
                false
            }
        }
    }

    /// Captures process paths, live PID, running state, and recent logs.
    #[must_use]
    pub fn snapshot(&self) -> CoreProcessSnapshot {
        self.try_snapshot().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to snapshot managed core process status");
            self.observed_snapshot(false, None, None)
        })
    }

    pub(crate) fn try_snapshot(&self) -> std::io::Result<CoreProcessSnapshot> {
        let (running, pid, started_at_secs) = {
            let mut child_slot = self.child.lock();
            match child_slot.as_mut() {
                Some(process) => match process.try_wait()? {
                    None => (
                        true,
                        Some(process.id()),
                        Some(self.started_at_secs.load(Ordering::Relaxed)).filter(|time| *time > 0),
                    ),
                    Some(status) => {
                        *self.last_exit_reason.write() = Some(format!("内核退出状态：{status}"));
                        self.started_at_secs.store(0, Ordering::Relaxed);
                        child_slot.take();
                        self.execution.lock().take();
                        (false, None, None)
                    }
                },
                None => (false, None, None),
            }
        };
        Ok(self.observed_snapshot(running, pid, started_at_secs))
    }

    fn observed_snapshot(
        &self,
        running: bool,
        pid: Option<u32>,
        started_at_secs: Option<u64>,
    ) -> CoreProcessSnapshot {
        CoreProcessSnapshot {
            kind: self.config.kind,
            running,
            pid,
            started_at_secs,
            exit_reason: self.last_exit_reason.read().clone(),
            binary: self.config.binary.clone(),
            config_file: self.config.config_file.clone(),
            home_dir: self.config.home_dir.clone(),
            logs: self.logs.read().recent_messages(),
        }
    }

    /// Terminates and reaps the child while retaining its handle on failure so
    /// callers may retry.
    ///
    /// # Errors
    ///
    /// Returns an error when status inspection, termination, or waiting fails.
    pub fn stop(&self) -> MihomoResult<()> {
        let mut child = self.child.lock();
        stop_child(&mut child)?;
        self.started_at_secs.store(0, Ordering::Relaxed);
        self.execution.lock().take();
        Ok(())
    }

    fn reserve_execution(&self) -> MihomoResult<Option<ExecutionReservation>> {
        #[cfg(test)]
        if let Some(slot) = &self.test_execution_slot {
            if slot.swap(true, Ordering::AcqRel) {
                return Err(MihomoError::Process(
                    "test core execution guard already held".into(),
                ));
            }
            return Ok(Some(ExecutionReservation::Test(TestExecutionGuard {
                held: slot.clone(),
            })));
        }
        reserve_native_execution(self.kind(), self.isolated_test_child)
    }

    /// Stops and reaps the managed child without blocking an async caller.
    ///
    /// # Errors
    ///
    /// Returns process or blocking-task failures. Repeated calls are safe.
    pub async fn stop_async(self: &Arc<Self>) -> MihomoResult<()> {
        let process = self.clone();
        tokio::task::spawn_blocking(move || process.stop())
            .await
            .map_err(|error| MihomoError::Process(format!("内核停止后台任务异常结束：{error}")))?
    }
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn launch_write_scopes(config: &CoreLaunchConfig) -> Vec<PathBuf> {
    let mut scopes = vec![config.home_dir.clone(), config.config_file.clone()];
    scopes.push(
        config
            .binary
            .parent()
            .map_or_else(|| config.binary.clone(), std::path::Path::to_path_buf),
    );
    if let Ok(binary) = std::fs::canonicalize(&config.binary)
        && let Some(parent) = binary.parent()
    {
        // Release staging and replacement use the executable's physical parent,
        // which can differ from the parent of its discovered symlink.
        scopes.push(parent.to_path_buf());
    }
    scopes
}

impl Drop for ManagedCoreProcess {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(gate) = self.drop_gate.get_mut().take() {
            let _ = gate.started.send(());
            // A panicking/cancelled test releases its RAII sender; timeout is the final bound.
            let _ = gate.resume.recv_timeout(Duration::from_secs(2));
        }
        if let Err(error) = self.stop() {
            tracing::warn!(%error, "failed to stop managed core while dropping process owner");
            // Never advertise a free kernel slot while termination is unconfirmed.
            if let Some(guard) = self.execution.get_mut().take()
                && let Some(mut child) = self.child.get_mut().take()
            {
                guard.release_after_exit(move || match child.try_wait() {
                    Ok(Some(_)) => true,
                    Ok(None) => {
                        let _ = child.kill();
                        child.wait().is_ok()
                    }
                    Err(_) => false,
                });
            }
        }
    }
}

impl CoreProcessLease for ManagedCoreProcess {
    fn kind(&self) -> CoreKind {
        self.kind()
    }

    fn endpoint(&self) -> MihomoEndpoint {
        self.endpoint().clone()
    }

    fn launch_descriptor(&self) -> CoreLaunchDescriptor {
        CoreLaunchDescriptor::from(self.launch_config())
    }

    fn write_scopes(&self) -> Vec<PathBuf> {
        self.write_scopes()
    }

    fn config_validator(&self) -> CoreConfigValidator {
        self.config_validator()
    }

    fn native_controller(
        &self,
    ) -> MihomoResult<Option<crate::runtime::service::ipc::NativeController>> {
        self.native_controller()
    }

    fn stop_process(&self) -> MihomoResult<()> {
        self.stop()
    }

    fn pid(&self) -> Option<u32> {
        self.child.lock().as_ref().map(Child::id)
    }

    fn is_running(&self) -> bool {
        self.is_running()
    }

    fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
}

fn reserve_native_execution(
    kind: CoreKind,
    isolated_test_child: bool,
) -> MihomoResult<Option<ExecutionReservation>> {
    // Unit fixtures intentionally run many isolated synthetic children in parallel.
    // Native reservation itself is exercised in zenclash-service and real-core tests.
    if kind != CoreKind::Mihomo || cfg!(test) || isolated_test_child {
        return Ok(None);
    }
    crate::runtime::service::ipc::CoreExecutionGuard::acquire()
        .map(ExecutionReservation::Native)
        .map(Some)
        .map_err(|error| MihomoError::Process(error.to_string()))
}
