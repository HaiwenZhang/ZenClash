// Forked from Clash Verge Rev core/service.rs, adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json.
//! Native helper operations use the upstream preparation/elevation contract.

use crate::health::PendingAction;
use anyhow::{Context as _, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// An OS cancellation is distinguished only when the OS returns its explicit cancellation code.
#[derive(Debug, thiserror::Error)]
pub enum MaintenanceError {
    #[error("administrator authorization was cancelled")]
    AuthorizationCancelled,
    #[error("service maintenance failed: {0:#}")]
    Failed(#[source] anyhow::Error),
    #[error("service maintenance worker did not complete: {0}")]
    OutcomeUnconfirmed(#[source] tokio::task::JoinError),
}

/// Runs one admitted operation on a worker. The app retains completion when its UI waiter closes.
///
/// # Errors
/// Returns explicit Windows authorization cancellation, installer errors or an unconfirmed worker completion.
pub async fn maintain_service(
    action: PendingAction,
    installer: PathBuf,
    core: Option<PathBuf>,
) -> Result<(), MaintenanceError> {
    tokio::task::spawn_blocking(move || {
        run_privileged(action, &installer, core.as_deref()).map_err(native_error)
    })
    .await
    .map_err(MaintenanceError::OutcomeUnconfirmed)?
}

/// Copied install/reinstall flow: one installer elevation replaces registration and approved cores.
///
/// # Errors
/// Returns missing or invalid sources, native authorization or installer-process errors; partial maintenance may have occurred.
pub fn run_privileged(action: PendingAction, installer: &Path, core: Option<&Path>) -> Result<()> {
    anyhow::ensure!(
        installer.is_absolute() && installer.is_file(),
        "packaged installer is unavailable"
    );
    if action == PendingAction::Uninstall {
        return uninstall_service(&installer.with_file_name(format!(
            "zenclash-service-uninstall{}",
            std::env::consts::EXE_SUFFIX
        )));
    }
    let core = core.context("selected core source is required for installation")?;
    let name = core
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid core file name")?
        .to_owned();
    #[cfg(unix)]
    // SAFETY: getgid reads the current native account's real group without side effects.
    let gid = Some(unsafe { libc::getgid() });
    #[cfg(windows)]
    let gid = None;
    zenclash_service::management::install(
        installer,
        &[zenclash_service::management::CoreSource {
            name,
            path: core.to_owned(),
        }],
        false,
        gid,
        "ZenClash needs administrator authorization to install its service",
    )
}

fn native_error(error: anyhow::Error) -> MaintenanceError {
    #[cfg(windows)]
    if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.raw_os_error() == Some(1223))
    {
        return MaintenanceError::AuthorizationCancelled;
    }
    MaintenanceError::Failed(error)
}

#[cfg(windows)]
fn uninstall_service(uninstaller: &Path) -> Result<()> {
    use deelevate::{PrivilegeLevel, Token};
    use runas::Command as RunasCommand;
    use std::os::windows::process::CommandExt as _;
    anyhow::ensure!(uninstaller.is_file(), "packaged uninstaller is unavailable");
    let token = Token::with_current_process()?;
    let status = match token.privilege_level()? {
        PrivilegeLevel::NotPrivileged => RunasCommand::new(uninstaller).show(false).status()?,
        _ => Command::new(uninstaller)
            .creation_flags(0x08000000)
            .status()?,
    };
    anyhow::ensure!(
        status.success(),
        "failed to uninstall service with status {}",
        status.code().unwrap_or(-1)
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_service(uninstaller: &Path) -> Result<()> {
    anyhow::ensure!(uninstaller.is_file(), "packaged uninstaller is unavailable");
    let status = if crate::current_process_elevated() {
        Command::new(uninstaller).status()?
    } else {
        // Keep the upstream pkexec -> sudo fallback while handling missing pkexec directly.
        match Command::new("pkexec")
            .arg("--disable-internal-agent")
            .arg(uninstaller)
            .status()
        {
            Ok(status) if status.success() => status,
            Ok(_) => Command::new("sudo").arg(uninstaller).status()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Command::new("sudo").arg(uninstaller).status()?
            }
            Err(error) => return Err(error.into()),
        }
    };
    anyhow::ensure!(
        status.success(),
        "failed to uninstall service with status {}",
        status.code().unwrap_or(-1)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_service(uninstaller: &Path) -> Result<()> {
    anyhow::ensure!(uninstaller.is_file(), "packaged uninstaller is unavailable");
    let shell = format!(
        "cd /; {}",
        shell_single_quote(&uninstaller.to_string_lossy())
    );
    // Pass the shell and prompt as separate argv, preventing quoting/injection through a localized prompt.
    let script = "on run argv\ndo shell script (item 1 of argv) with administrator privileges with prompt (item 2 of argv)\nend run";
    let status = Command::new("osascript")
        .args([
            "-e",
            script,
            &shell,
            "ZenClash needs administrator authorization to remove its service",
        ])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "failed to uninstall service with status {}",
        status.code().unwrap_or(-1)
    );
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_path_preserves_spaces_quotes_and_literal_shell_characters() {
        assert_eq!(
            shell_single_quote("/a b/c'd$(touch x)"),
            "'/a b/c'\\''d$(touch x)'"
        );
    }
    #[cfg(windows)]
    #[test]
    fn only_explicit_windows_authorization_cancellation_is_reported_as_cancelled() {
        assert!(matches!(
            native_error(std::io::Error::from_raw_os_error(1223).into()),
            MaintenanceError::AuthorizationCancelled
        ));
        for code in [5, 126, 127] {
            assert!(matches!(
                native_error(std::io::Error::from_raw_os_error(code).into()),
                MaintenanceError::Failed(_)
            ));
        }
        assert!(matches!(
            native_error(anyhow::anyhow!("installer exited with status 1223")),
            MaintenanceError::Failed(_)
        ));
    }

    #[test]
    fn a_missing_helper_is_rejected_before_elevation() {
        assert!(
            run_privileged(PendingAction::Uninstall, Path::new("missing-helper"), None).is_err()
        );
    }
}
