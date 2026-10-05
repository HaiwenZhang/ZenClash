// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json.
//! Native registration probes copied from upstream core/service.rs.
use anyhow::{Context as _, Result};
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "linux")]
use {anyhow::bail, std::process::Command as StdCommand};

#[cfg(target_os = "macos")]
fn path_entry_exists_without_follow(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn macos_service_install_markers() -> Vec<String> {
    vec![
        format!(
            "/Library/LaunchDaemons/{}.plist",
            zenclash_service::MACOS_SERVICE_ID
        ),
        format!(
            "/Library/PrivilegedHelperTools/{}.bundle",
            zenclash_service::MACOS_SERVICE_ID
        ),
    ]
}

#[cfg(target_os = "macos")]
fn macos_service_install_marker_exists() -> std::io::Result<bool> {
    for marker in macos_service_install_markers() {
        if path_entry_exists_without_follow(Path::new(&marker))? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(windows)]
fn open_registered_service() -> Result<Option<windows_service::service::Service>> {
    use windows_service::{
        Error as WindowsServiceError,
        service::ServiceAccess,
        service_manager::{ServiceManager as WindowsServiceManager, ServiceManagerAccess},
    };

    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    let manager =
        WindowsServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(
        zenclash_service::WINDOWS_SERVICE_NAME,
        ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => Ok(Some(service)),
        Err(WindowsServiceError::Winapi(error))
            if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("failed to inspect Windows service registration"),
    }
}

#[cfg(windows)]
///
/// # Errors
/// Returns errors reading native registration or helper evidence. Unreadable evidence is not treated as absence.
pub fn trusted_service_evidence() -> Result<bool> {
    Ok(open_registered_service()?.is_some())
}

/// Whether IPC cannot succeed until the service is started again. A service that is starting, or
/// that the SCM has not started yet this boot, is left to the IPC retries, which wait for it.
#[cfg(windows)]
///
/// # Errors
/// Returns SCM connection, registration or status-query errors.
pub fn service_stopped() -> Result<bool> {
    use windows_service::service::{ServiceExitCode, ServiceState};

    const ERROR_SERVICE_NEVER_STARTED: u32 = 1077;
    let Some(service) = open_registered_service()? else {
        return Ok(true);
    };
    let status = service
        .query_status()
        .context("failed to query Windows service status")?;
    Ok(status.current_state == ServiceState::Stopped
        && status.exit_code != ServiceExitCode::Win32(ERROR_SERVICE_NEVER_STARTED))
}

#[cfg(target_os = "linux")]
pub fn trusted_service_evidence() -> Result<bool> {
    let unit = format!("{}.service", zenclash_service::SERVICE_SLUG);
    let output = StdCommand::new("systemctl")
        .args(["show", "--property=LoadState", "--value", &unit])
        .output()
        .context("failed to inspect systemd service registration")?;
    if !output.status.success() {
        bail!(
            "systemd service registration probe failed with status {}",
            output.status
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() != "not-found")
}

#[cfg(target_os = "macos")]
pub fn trusted_service_evidence() -> Result<bool> {
    macos_service_install_marker_exists().context("failed to inspect launchd service registration")
}

/// Uses the application's upstream retry policy; call during bootstrap before IPC operations.
pub const fn application_ipc_config() -> zenclash_service::IpcConfig {
    zenclash_service::IpcConfig {
        default_timeout: std::time::Duration::from_millis(1000),
        retry_delay: std::time::Duration::from_millis(500),
        max_retries: 20,
    }
}
