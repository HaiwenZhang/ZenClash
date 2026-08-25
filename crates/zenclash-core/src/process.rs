use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use parking_lot::{Mutex, RwLock};

use crate::{MihomoClient, MihomoEndpoint, MihomoError, MihomoResult};

mod discovery;
mod resources;

#[cfg(test)]
mod tests;

pub use discovery::MihomoLaunchConfig;

const MAX_LOG_LINES: usize = 1_000;

/// Owned managed Mihomo child process with bounded stdout/stderr history.
pub struct MihomoProcess {
    child: Mutex<Option<Child>>,
    logs: Arc<RwLock<VecDeque<String>>>,
    config: MihomoLaunchConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Point-in-time state used by the native runtime page.
pub struct MihomoProcessSnapshot {
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
        std::fs::create_dir_all(&config.home_dir)
            .map_err(|error| MihomoError::Process(error.to_string()))?;
        let mut command = Command::new(&config.binary);
        configure_child_command(&mut command);
        command
            .arg("-d")
            .arg(&config.home_dir)
            .arg("-f")
            .arg(&config.config_file);
        if let Some(controller) = &config.controller_override {
            command.arg("-ext-ctl").arg(controller);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                MihomoError::Process(format!("无法启动 {}：{error}", config.binary.display()))
            })?;

        let logs = Arc::new(RwLock::new(VecDeque::new()));
        let collectors = (|| {
            if let Some(stdout) = child.stdout.take() {
                collect_output(stdout, "INFO", logs.clone())?;
            }
            if let Some(stderr) = child.stderr.take() {
                collect_output(stderr, "CORE", logs.clone())?;
            }
            Ok::<_, MihomoError>(())
        })();
        if let Err(error) = collectors {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        Ok(Arc::new(Self {
            child: Mutex::new(Some(child)),
            logs,
            config,
        }))
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
            if !self.is_running() {
                let message = self
                    .logs
                    .read()
                    .back()
                    .cloned()
                    .unwrap_or_else(|| "mihomo 在控制器就绪前退出".into());
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
            "等待 Mihomo 控制器 {} 超时",
            self.config.endpoint.controller
        ))
    }

    /// Returns the controller endpoint assigned to this process.
    #[must_use]
    pub const fn endpoint(&self) -> &MihomoEndpoint {
        &self.config.endpoint
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
        let Some(process) = child.as_mut() else {
            return Ok(());
        };
        if process
            .try_wait()
            .map_err(|error| MihomoError::Process(format!("查询 Mihomo 状态失败：{error}")))?
            .is_none()
        {
            process
                .kill()
                .map_err(|error| MihomoError::Process(format!("停止 Mihomo 失败：{error}")))?;
            process
                .wait()
                .map_err(|error| MihomoError::Process(format!("等待 Mihomo 退出失败：{error}")))?;
        }
        child.take();
        drop(child);
        Ok(())
    }
}

fn readiness_attempt_timeout(remaining: Duration) -> Duration {
    remaining.min(Duration::from_millis(750))
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
        .name(format!("zenclash-mihomo-{label}"))
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
