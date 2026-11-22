// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Core child process guard, stdout/stderr log capture, and graceful termination.

use super::ipc_security::secure_core_ipc_socket;
use super::{CORE_OUTPUT_DRAIN_TIMEOUT, CORE_TERM_GRACE};
use crate::core::ClashConfig;
use crate::core::logger::{LOG_RING, set_or_update_writer, write_core_line};
use crate::{OwnerIdentity, WriterConfig};
use anyhow::{Context as _, Result, anyhow};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Grace left before `kill_by`, evaluated when the core is actually stopped; `None` is the full grace.
pub(super) fn term_grace(kill_by: Option<Instant>) -> Duration {
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

#[cfg(windows)]
struct ChildJobObject {
    _handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl ChildJobObject {
    fn assign_child(child: &Child) -> Result<Self> {
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        };

        let process_handle = child
            .raw_handle()
            .context("spawned Windows core process handle is unavailable")?;
        // SAFETY: Both arguments are null, requesting an anonymous Job Object with default security;
        // the returned handle is checked for null before adoption.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(std::io::Error::last_os_error()).context("failed to create Windows Job Object for core");
        }
        // SAFETY: `raw_job` is non-null and exclusively owned by this guard.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `job` is a live Job Object handle and `limits` is initialized storage matching the size passed.
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to configure JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE on Windows Job Object");
        }

        // SAFETY: Both `job` and `process_handle` are valid live OS handles for this synchronous call.
        let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), process_handle) };
        if assigned == 0 {
            return Err(std::io::Error::last_os_error()).context("failed to assign Windows core process to Job Object");
        }

        Ok(Self { _handle: job })
    }
}

pub struct ChildGuard {
    pub(super) child: Option<Child>,
    pub(super) readers: Vec<JoinHandle<()>>,
    #[cfg(windows)]
    job: Option<ChildJobObject>,
}

impl ChildGuard {
    #[cfg(test)]
    pub(super) fn from_readers(readers: Vec<JoinHandle<()>>) -> Self {
        Self {
            child: None,
            readers,
            #[cfg(windows)]
            job: None,
        }
    }

    pub(super) fn inner(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    pub(super) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    pub(super) fn take(mut self) -> Option<Child> {
        self.child.take()
    }

    pub(super) async fn await_ipc(&mut self, path: String, owner: OwnerIdentity) -> Result<()> {
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
    pub(super) async fn terminate(&mut self, grace: Duration) -> Result<()> {
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
        #[cfg(windows)]
        {
            self.job.take();
        }
        self.drain_output().await;
        info!("Core ({:?}) stopped", child_id);
        Ok(())
    }

    pub(super) async fn drain_output(&mut self) {
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
        // SAFETY: `kill` only signals a PID owned by this child process and
        // has no memory preconditions.
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

pub(super) fn core_args(config: &ClashConfig) -> Vec<String> {
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

pub(super) fn log_core_exit(status: &std::process::ExitStatus, uptime: Duration) -> String {
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
    let (child, job) = {
        let OwnerIdentity::Windows { sid } = owner else {
            return Err(anyhow!("Windows core requires a Windows owner identity"));
        };
        let mut child = Command::new(bin_path)
            .args(args)
            .env(
                "LISTEN_NAMEDPIPE_SDDL",
                super::ipc_security::windows_owner_pipe_sddl(sid),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let job = match ChildJobObject::assign_child(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };
        (child, Some(job))
    };

    // SAFETY: `pre_exec` runs only `umask`, which is async-signal-safe and
    // does not touch shared Rust state before exec.
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
        #[cfg(windows)]
        job,
    };

    let (Some(stdout), Some(stderr)) = (
        child_guard.inner().and_then(|c| c.stdout.take()),
        child_guard.inner().and_then(|c| c.stderr.take()),
    ) else {
        return Err(anyhow!("Failed to capture child output"));
    };

    child_guard
        .readers
        .push(spawn_line_reader(stdout, tracing::Level::INFO));
    child_guard
        .readers
        .push(spawn_line_reader(stderr, tracing::Level::ERROR));

    Ok(child_guard)
}

fn spawn_line_reader<R>(reader: R, level: tracing::Level) -> JoinHandle<()>
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
