//! Child process spawning, termination, and stdout/stderr log collection.

#[cfg(unix)]
use std::time::Duration;
use std::{
    process::{Child, Command, Stdio},
    sync::Arc,
};

use parking_lot::RwLock;

use super::{
    CoreLaunchConfig, input,
    launch_logs::{self, LaunchLogHistory, LaunchLogIdentity},
};
use crate::{CoreKind, MihomoError, MihomoResult};

#[cfg(unix)]
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn stop_child(child: &mut Option<Child>) -> MihomoResult<()> {
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
pub(super) fn stop_running_child(process: &mut Child) -> MihomoResult<()> {
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
pub(super) fn stop_running_child(process: &mut Child) -> MihomoResult<()> {
    process
        .kill()
        .map_err(|error| MihomoError::Process(format!("停止内核失败：{error}")))?;
    process
        .wait()
        .map_err(|error| MihomoError::Process(format!("等待内核退出失败：{error}")))?;
    Ok(())
}

pub(super) fn spawn_child(
    config: &CoreLaunchConfig,
    logs: Arc<RwLock<LaunchLogHistory>>,
    input: Option<std::fs::File>,
    recovery_asset_root: Option<&std::path::Path>,
) -> MihomoResult<Child> {
    std::fs::create_dir_all(&config.home_dir)
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    let mut command = Command::new(&config.binary);
    configure_child_command(&mut command);
    if config.kind == CoreKind::Mihomo {
        crate::mihomo::config::validation::configure_recovery_assets(
            &mut command,
            recovery_asset_root,
        )
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    }
    command.arg("-d").arg(&config.home_dir).arg("-f");
    if let Some(input) = input {
        // Mihomo's config-string environment takes precedence over -f, including stdin.
        command
            .env_remove("CLASH_CONFIG_STRING")
            .arg("-")
            .stdin(Stdio::from(input));
    } else {
        command.arg(&config.config_file);
    }
    if let Some(path) = config.endpoint.ipc_path() {
        command
            .arg(if cfg!(windows) {
                "-ext-ctl-pipe"
            } else {
                "-ext-ctl-unix"
            })
            .arg(path)
            .arg("--secret")
            .arg(&config.endpoint.secret);
        #[cfg(windows)]
        command.env(
            "LISTEN_NAMEDPIPE_SDDL",
            crate::runtime::service::ipc::current_user_pipe_sddl()
                .map_err(|error| MihomoError::Process(error.to_string()))?,
        );
    } else if let Some(controller) = &config.controller_override {
        command
            .arg("--ext-ctl")
            .arg(controller)
            .arg("--secret")
            .arg(&config.endpoint.secret);
    }
    let launch = logs.write().begin_launch();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MihomoError::Process(format!("无法启动 {}：{error}", config.binary.display()))
        })?;
    let collectors = (|| {
        if let Some(stdout) = child.stdout.take() {
            collect_output(stdout, "INFO", logs.clone(), launch.clone())?;
        }
        if let Some(stderr) = child.stderr.take() {
            collect_output(stderr, "CORE", logs, launch)?;
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

pub(super) fn local_launch_payload(config: &CoreLaunchConfig) -> MihomoResult<Option<String>> {
    if config.kind != CoreKind::Mihomo {
        return Ok(None);
    }
    let payload = input::read_config(&config.config_file)?;
    crate::mihomo::tun::admission::ensure_local_yaml(config.kind, &payload)?;
    Ok(Some(payload))
}

#[cfg(target_os = "windows")]
fn configure_child_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_child_command(_command: &mut Command) {}

pub(super) fn collect_output(
    reader: impl std::io::Read + Send + 'static,
    label: &'static str,
    logs: Arc<RwLock<LaunchLogHistory>>,
    launch: LaunchLogIdentity,
) -> MihomoResult<()> {
    std::thread::Builder::new()
        .name(format!("zenclash-core-{label}"))
        .spawn(move || {
            if let Err(error) = launch_logs::read_output(reader, |line| {
                logs.write().record(&launch, label, line);
            }) {
                tracing::warn!(%error, stream = label, "failed to read managed core output");
            }
        })
        .map(|_| ())
        .map_err(|error| {
            MihomoError::Process(format!("无法启动内核 {label} 日志收集线程：{error}"))
        })
}
