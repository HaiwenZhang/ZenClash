use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use parking_lot::{Mutex, RwLock};

use crate::{
    CoreCapabilities, CoreConfigValidator, CoreKind, MihomoClient, MihomoEndpoint, MihomoError,
    MihomoResult,
};

mod discovery;
mod resources;

#[cfg(test)]
mod tests;

pub use discovery::MihomoLaunchConfig;
pub use resources::bundled_recovery_profile;

const MAX_LOG_LINES: usize = 1_000;
#[cfg(unix)]
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_MEOW_RUST_LOG: &str = "info";
const QUIET_MEOW_PROTOCOL_LOGS: &str = "tokio_tungstenite=warn,tungstenite=warn";

/// Owned managed Mihomo child process with bounded stdout/stderr history.
pub struct MihomoProcess {
    child: Mutex<Option<Child>>,
    logs: Arc<RwLock<VecDeque<String>>>,
    config: MihomoLaunchConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Point-in-time state used by the native runtime page.
pub struct MihomoProcessSnapshot {
    /// Runtime core implementation represented by this process.
    pub kind: CoreKind,
    /// Whether the child is still running.
    pub running: bool,
    /// Live process identifier, absent after exit or stop.
    pub pid: Option<u32>,
    /// Executable used to launch the child.
    pub binary: PathBuf,
    /// Active YAML configuration passed to the child.
    pub config_file: PathBuf,
    /// Mihomo writable data directory.
    pub home_dir: PathBuf,
    /// Most recent bounded stdout and stderr lines.
    pub logs: Vec<String>,
}

impl MihomoProcess {
    /// Starts Mihomo and attaches bounded output collectors.
    ///
    /// # Errors
    ///
    /// Returns an error when the data directory, child process, or collector
    /// threads cannot be created. A partially started child is terminated.
    pub fn spawn(config: MihomoLaunchConfig) -> MihomoResult<Arc<Self>> {
        let logs = Arc::new(RwLock::new(VecDeque::new()));
        let child = spawn_child(&config, logs.clone())?;

        Ok(Arc::new(Self {
            child: Mutex::new(Some(child)),
            logs,
            config,
        }))
    }

    /// Stops the current child and starts the same binary and configuration.
    ///
    /// Existing bounded logs remain available and new stdout/stderr collectors
    /// append to the same history.
    ///
    /// # Errors
    ///
    /// Returns an error when the old process cannot be stopped or the new child
    /// and its output collectors cannot be started. After a spawn failure the
    /// process remains stopped so callers can report and retry safely.
    pub fn restart(&self) -> MihomoResult<()> {
        self.config.validate_config().map_err(|error| {
            MihomoError::Process(format!("重启前配置预检失败，当前内核保持运行：{error}"))
        })?;
        let mut child_slot = self.child.lock();
        stop_child(&mut child_slot)?;
        let child = spawn_child(&self.config, self.logs.clone())?;
        *child_slot = Some(child);
        Ok(())
    }

    /// Restarts the managed child away from an async caller and waits for its controller.
    ///
    /// This is the single lifecycle transition for runtime callers. Blocking
    /// process operations run on Tokio's blocking pool before readiness is
    /// checked asynchronously.
    ///
    /// # Errors
    ///
    /// Returns validation, stop, spawn, task, early-exit, or readiness errors.
    pub async fn restart_and_wait(self: &Arc<Self>, timeout: Duration) -> MihomoResult<()> {
        let process = self.clone();
        tokio::task::spawn_blocking(move || process.restart())
            .await
            .map_err(|error| {
                MihomoError::Process(format!("内核重启后台任务异常结束：{error}"))
            })??;
        self.wait_until_ready(timeout).await
    }

    /// Polls the real `/version` endpoint until it responds, the child exits,
    /// or the supplied timeout elapses.
    ///
    /// # Errors
    ///
    /// Returns the last process log when Mihomo exits early, or a timeout error.
    pub async fn wait_until_ready(&self, timeout: Duration) -> MihomoResult<()> {
        let client = MihomoClient::new(self.config.endpoint.clone())?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.readiness_timeout_error());
            }
            if tokio::time::timeout(readiness_attempt_timeout(remaining), client.version())
                .await
                .is_ok_and(|result| result.is_ok())
            {
                return Ok(());
            }
            if let Some(error) = controller_listener_error(&self.logs.read()) {
                return Err(MihomoError::Process(error));
            }
            if !self.is_running() {
                let message = self.logs.read().back().cloned().unwrap_or_else(|| {
                    format!("{} 在控制器就绪前退出", self.config.kind.display_name())
                });
                return Err(MihomoError::Process(message));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.readiness_timeout_error());
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    fn readiness_timeout_error(&self) -> MihomoError {
        MihomoError::Process(format!(
            "等待 {} 控制器 {} 超时",
            self.config.kind.display_name(),
            self.config.endpoint.controller
        ))
    }

    /// Returns the controller endpoint assigned to this process.
    #[must_use]
    pub const fn endpoint(&self) -> &MihomoEndpoint {
        &self.config.endpoint
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
    }

    /// Checks whether the child has not exited yet.
    #[must_use]
    pub fn is_running(&self) -> bool {
        let mut child = self.child.lock();
        child.as_mut().is_some_and(|child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(error) => {
                tracing::warn!(%error, "failed to query Mihomo process status");
                false
            }
        })
    }

    /// Captures process paths, live PID, running state, and recent logs.
    #[must_use]
    pub fn snapshot(&self) -> MihomoProcessSnapshot {
        let (running, pid) = {
            let mut child = self.child.lock();
            child
                .as_mut()
                .map_or((false, None), |process| match process.try_wait() {
                    Ok(None) => (true, Some(process.id())),
                    Ok(Some(_)) => (false, None),
                    Err(error) => {
                        tracing::warn!(%error, "failed to snapshot Mihomo process status");
                        (false, None)
                    }
                })
        };
        MihomoProcessSnapshot {
            kind: self.config.kind,
            running,
            pid,
            binary: self.config.binary.clone(),
            config_file: self.config.config_file.clone(),
            home_dir: self.config.home_dir.clone(),
            logs: self.logs.read().iter().cloned().collect(),
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
        stop_child(&mut child)
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

fn controller_listener_error(logs: &VecDeque<String>) -> Option<String> {
    logs.iter().rev().find_map(|line| {
        let normalized = line.to_ascii_lowercase();
        (normalized.contains("external controller listen error")
            && normalized.contains("address already in use"))
        .then(|| line.clone())
    })
}

fn stop_child(child: &mut Option<Child>) -> MihomoResult<()> {
    let Some(process) = child.as_mut() else {
        return Ok(());
    };
    if process
        .try_wait()
        .map_err(|error| MihomoError::Process(format!("查询内核状态失败：{error}")))?
        .is_none()
    {
        stop_running_child(process)?;
    }
    child.take();
    Ok(())
}

#[cfg(unix)]
fn stop_running_child(process: &mut Child) -> MihomoResult<()> {
    let pid = libc::pid_t::try_from(process.id())
        .map_err(|_| MihomoError::Process("内核进程 ID 超出平台范围".into()))?;
    // SAFETY: `pid` comes from the live `Child` handle and SIGTERM does not
    // access memory in this process.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let error = std::io::Error::last_os_error();
        if process
            .try_wait()
            .map_err(|status| MihomoError::Process(format!("查询内核状态失败：{status}")))?
            .is_some()
        {
            return Ok(());
        }
        return Err(MihomoError::Process(format!(
            "向内核发送 SIGTERM 失败：{error}"
        )));
    }

    let deadline = std::time::Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
    loop {
        if process
            .try_wait()
            .map_err(|error| MihomoError::Process(format!("等待内核退出失败：{error}")))?
            .is_some()
        {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
    }

    tracing::warn!(pid, "managed core ignored SIGTERM; forcing termination");
    process
        .kill()
        .map_err(|error| MihomoError::Process(format!("强制停止内核失败：{error}")))?;
    process
        .wait()
        .map_err(|error| MihomoError::Process(format!("等待强制停止的内核退出失败：{error}")))?;
    Ok(())
}

#[cfg(not(unix))]
fn stop_running_child(process: &mut Child) -> MihomoResult<()> {
    process
        .kill()
        .map_err(|error| MihomoError::Process(format!("停止内核失败：{error}")))?;
    process
        .wait()
        .map_err(|error| MihomoError::Process(format!("等待内核退出失败：{error}")))?;
    Ok(())
}

fn readiness_attempt_timeout(remaining: Duration) -> Duration {
    remaining.min(Duration::from_millis(750))
}

fn spawn_child(
    config: &MihomoLaunchConfig,
    logs: Arc<RwLock<VecDeque<String>>>,
) -> MihomoResult<Child> {
    std::fs::create_dir_all(&config.home_dir)
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    let mut command = Command::new(&config.binary);
    configure_child_command(&mut command);
    configure_child_environment(&mut command, config.kind);
    command
        .arg("-d")
        .arg(&config.home_dir)
        .arg("-f")
        .arg(&config.config_file);
    if let Some(controller) = &config.controller_override {
        command
            .arg("--ext-ctl")
            .arg(controller)
            .arg("--secret")
            .arg(&config.endpoint.secret);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MihomoError::Process(format!("无法启动 {}：{error}", config.binary.display()))
        })?;
    let collectors = (|| {
        if let Some(stdout) = child.stdout.take() {
            collect_output(stdout, "INFO", logs.clone())?;
        }
        if let Some(stderr) = child.stderr.take() {
            collect_output(stderr, "CORE", logs)?;
        }
        Ok::<_, MihomoError>(())
    })();
    if let Err(error) = collectors {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

fn configure_child_environment(command: &mut Command, kind: CoreKind) {
    let Some(filter) = core_rust_log_filter(
        kind,
        std::env::var("ZENCLASH_MEOW_RUST_LOG").ok().as_deref(),
    ) else {
        return;
    };
    command.env("RUST_LOG", filter);
}

fn core_rust_log_filter(kind: CoreKind, requested: Option<&str>) -> Option<String> {
    if kind != CoreKind::Meow {
        return None;
    }
    let requested = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MEOW_RUST_LOG);
    Some(format!("{requested},{QUIET_MEOW_PROTOCOL_LOGS}"))
}

#[cfg(target_os = "windows")]
fn configure_child_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_child_command(_command: &mut Command) {}

impl Drop for MihomoProcess {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::warn!(%error, "failed to stop Mihomo while dropping process owner");
        }
    }
}

fn collect_output(
    reader: impl std::io::Read + Send + 'static,
    label: &'static str,
    logs: Arc<RwLock<VecDeque<String>>>,
) -> MihomoResult<()> {
    std::thread::Builder::new()
        .name(format!("zenclash-core-{label}"))
        .spawn(move || {
            for line in BufReader::new(reader).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        tracing::warn!(%error, stream = label, "failed to read Mihomo output");
                        break;
                    }
                };
                let mut logs = logs.write();
                if logs.len() >= MAX_LOG_LINES {
                    logs.pop_front();
                }
                logs.push_back(format!("[{label}] {line}"));
                drop(logs);
            }
        })
        .map(|_| ())
        .map_err(|error| {
            MihomoError::Process(format!("无法启动 Mihomo {label} 日志收集线程：{error}"))
        })
}
