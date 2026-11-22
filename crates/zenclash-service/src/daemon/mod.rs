#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Cross-platform IPC daemon lifecycle and unified `run | install | uninstall` CLI dispatch.

use std::ffi::OsString;

use anyhow::{Context as _, Result, bail};
use tracing::{Level, error, info, warn};
use tracing_subscriber::FmtSubscriber;

use crate::{
    acquire_service_owner, reconcile_service_startup, restore_desired_state, run_ipc_supervisor_until_shutdown,
};

#[cfg(windows)]
use {
    platform_lib::{
        define_windows_service,
        service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType},
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    },
    std::time::Duration,
};

/// Parsed subcommand for the unified `zenclash-service` binary.
#[derive(Debug, PartialEq, Eq)]
pub enum CliCommand {
    /// Print build version and exit without starting the daemon.
    Version,
    /// Run the privileged IPC daemon (default when no subcommand is given).
    Run,
    /// Run privileged service/core installation with the forwarded arguments.
    Install(Vec<OsString>),
    /// Run privileged service/core uninstallation with the forwarded arguments.
    Uninstall(Vec<OsString>),
}

/// Parses arguments passed to `zenclash-service` (excluding `argv[0]`).
///
/// # Errors
/// Returns an error if an unrecognized subcommand or argument is supplied.
pub fn parse_cli_command<I>(arguments: I) -> Result<CliCommand>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = arguments.into_iter();
    let Some(first) = args.next() else {
        return Ok(CliCommand::Run);
    };
    match first.to_str() {
        Some("--version" | "-V") => Ok(CliCommand::Version),
        Some("run") => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `run`: {extra:?}");
            }
            Ok(CliCommand::Run)
        }
        Some("install") => Ok(CliCommand::Install(args.collect())),
        Some("uninstall") => Ok(CliCommand::Uninstall(args.collect())),
        _ => bail!("unknown subcommand or argument {first:?}; expected `run`, `install`, `uninstall`, or `--version`"),
    }
}

/// Entrypoint for the unified `zenclash-service` binary.
///
/// # Errors
/// Returns CLI parsing, installation, uninstallation, or daemon runtime errors.
pub fn run_cli() -> Result<()> {
    match parse_cli_command(std::env::args_os().skip(1))? {
        CliCommand::Version => {
            println!("zenclash-service {}", crate::VERSION);
            Ok(())
        }
        CliCommand::Run => run_daemon(),
        CliCommand::Install(args) => crate::install::run_install_cli_with_args(args),
        CliCommand::Uninstall(args) => crate::install::run_uninstall_cli_with_args(args),
    }
}

/// Runs the privileged IPC daemon (as Windows SCM service when possible, otherwise standalone).
///
/// # Errors
/// Returns runtime initialization, owner lock, or IPC supervisor errors.
pub fn run_daemon() -> Result<()> {
    #[cfg(unix)]
    set_secure_process_umask();

    init_logger();

    #[cfg(windows)]
    {
        if service_dispatcher::start(crate::WINDOWS_SERVICE_NAME, ffi_service_main).is_err() {
            info!("Not running as a service, starting in standalone mode.");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build standalone tokio runtime")?;
            rt.block_on(run_standalone())?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to build standalone tokio runtime")?;
        rt.block_on(run_standalone())
    }
}

#[cfg(unix)]
fn set_secure_process_umask() {
    // SAFETY: `umask` sets the process file creation mask and has no pointer or memory preconditions.
    unsafe {
        platform_lib::umask(0o077);
    }
}

#[cfg(windows)]
define_windows_service!(ffi_service_main, my_service_main);

#[cfg(windows)]
fn my_service_main(_args: Vec<OsString>) {
    if let Err(error) = run_service() {
        error!("Service failed to run: {error}");
    }
}

#[cfg(windows)]
fn run_service() -> platform_lib::Result<()> {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                crate::windows_service_begin_stopping();
                let _ = shutdown_tx.try_send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::TriggerEvent => {
                if crate::windows_service_is_stopping() {
                    // Ask SCM to queue this wake until the current instance has exited.
                    ServiceControlHandlerResult::Other(windows_sys::Win32::Foundation::ERROR_SHUTDOWN_IN_PROGRESS)
                } else {
                    ServiceControlHandlerResult::NoError
                }
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(crate::WINDOWS_SERVICE_NAME, event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP
            | ServiceControlAccept::SHUTDOWN
            | ServiceControlAccept::TRIGGER_EVENT,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    let fatal = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(async {
            let owner_guard = match acquire_service_owner().await {
                Ok(Some(owner_guard)) => owner_guard,
                Ok(None) => return false,
                Err(error) => {
                    warn!("Failed to acquire service owner lock: {error}");
                    return true;
                }
            };

            match reconcile_service_startup().await {
                Ok(()) => {
                    if let Err(error) = restore_desired_state().await {
                        warn!("Desired state restoration failed; keeping IPC available for GUI recovery: {error:#}");
                    }
                }
                Err(error) => warn!(
                    "Service startup reconciliation failed; core starts remain blocked while IPC is available: {error}"
                ),
            }

            let result = run_ipc_supervisor_until_shutdown(async {
                let _ = shutdown_rx.recv().await;
            })
            .await;
            if let Err(error) = result {
                warn!("IPC supervisor failed: {error}");
                drop(owner_guard);
                return true;
            }

            drop(owner_guard);
            false
        }),
        Err(error) => {
            error!("Failed to initialize Windows service tokio runtime: {error}");
            true
        }
    };

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(if fatal { 1 } else { 0 }),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    if fatal {
        std::process::exit(1);
    }

    Ok(())
}

fn init_logger() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

async fn run_standalone() -> Result<()> {
    let pid = std::process::id();
    info!("ZenClash Service - Standalone Mode");
    info!("Current process PID: {pid}");

    let Some(_owner_guard) = acquire_service_owner().await? else {
        return Ok(());
    };

    // Startup recovery is best-effort so stale desired state cannot prevent IPC self-healing.
    match reconcile_service_startup().await {
        Ok(()) => {
            if let Err(error) = restore_desired_state().await {
                warn!(
                    "Failed to restore desired core state on startup; core will not be auto-started. \
                     Keeping the IPC server up so the GUI can reconnect and recover: {error:#}"
                );
            }
        }
        Err(error) => {
            warn!("Service startup reconciliation failed; core starts remain blocked while IPC is available: {error:#}")
        }
    }

    run_ipc_supervisor_until_shutdown(shutdown_signal()).await?;

    info!("Service shutdown complete.");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(sig) => Some(sig),
            Err(error) => {
                error!("Failed to install SIGINT handler: {error}");
                None
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(sig) => Some(sig),
            Err(error) => {
                error!("Failed to install SIGTERM handler: {error}");
                None
            }
        };

        let recv_int = async {
            match sigint.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let recv_term = async {
            match sigterm.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            () = recv_int => info!("Received SIGINT (Ctrl+C)"),
            () = recv_term => info!("Received SIGTERM"),
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

        let mut sig_c = ctrl_c().ok();
        let mut sig_break = ctrl_break().ok();
        let mut sig_close = ctrl_close().ok();
        let mut sig_logoff = ctrl_logoff().ok();
        let mut sig_shutdown = ctrl_shutdown().ok();

        let wait_c = async {
            match sig_c.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let wait_break = async {
            match sig_break.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let wait_close = async {
            match sig_close.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let wait_logoff = async {
            match sig_logoff.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let wait_shutdown = async {
            match sig_shutdown.as_mut() {
                Some(sig) => {
                    let _ = sig.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            () = wait_c => info!("Received Ctrl+C"),
            () = wait_break => info!("Received Ctrl+Break"),
            () = wait_close => info!("Received console close"),
            () = wait_logoff => info!("Received logoff"),
            () = wait_shutdown => info!("Received system shutdown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CliCommand, parse_cli_command};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> anyhow::Result<CliCommand> {
        parse_cli_command(args.iter().map(OsString::from))
    }

    #[test]
    fn cli_defaults_to_run_and_dispatches_subcommands() -> anyhow::Result<()> {
        assert_eq!(parse(&[])?, CliCommand::Run);
        assert_eq!(parse(&["run"])?, CliCommand::Run);
        assert_eq!(parse(&["--version"])?, CliCommand::Version);
        assert_eq!(parse(&["-V"])?, CliCommand::Version);
        assert_eq!(
            parse(&["install", "--debug", "--install-service"])?,
            CliCommand::Install(vec![OsString::from("--debug"), OsString::from("--install-service")])
        );
        assert_eq!(
            parse(&["uninstall", "--debug"])?,
            CliCommand::Uninstall(vec![OsString::from("--debug")])
        );
        assert!(parse(&["run", "--unexpected"]).is_err());
        assert!(parse(&["unknown-command"]).is_err());
        Ok(())
    }
}
