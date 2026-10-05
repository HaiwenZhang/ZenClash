// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Cross-platform IPC daemon, run standalone or as a Windows service.

use anyhow::Result;
use tracing::{Level, info, warn};
use tracing_subscriber::FmtSubscriber;
use zenclash_service::{
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
    std::ffi::OsString,
    std::time::Duration,
};

#[cfg(not(windows))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    if print_version_if_requested() {
        return Ok(());
    }
    set_secure_process_umask();
    init_logger();
    run_standalone().await
}

#[cfg(unix)]
fn set_secure_process_umask() {
    unsafe {
        platform_lib::umask(0o077);
    }
}

/// Runs as a Windows service when possible, otherwise standalone.
#[cfg(windows)]
fn main() -> Result<()> {
    if print_version_if_requested() {
        return Ok(());
    }
    init_logger();
    if service_dispatcher::start(zenclash_service::WINDOWS_SERVICE_NAME, ffi_service_main).is_err() {
        info!("Not running as a service, starting in standalone mode.");
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(run_standalone())?;
    }
    Ok(())
}

#[cfg(windows)]
define_windows_service!(ffi_service_main, my_service_main);

#[cfg(windows)]
fn my_service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        info!("Service failed to run: {}", e);
    }
}

#[cfg(windows)]
fn run_service() -> platform_lib::Result<()> {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.try_send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(zenclash_service::WINDOWS_SERVICE_NAME, event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fatal = rt.block_on(async {
        let owner_guard = match acquire_service_owner().await {
            Ok(Some(owner_guard)) => owner_guard,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!("Failed to acquire service owner lock: {}", error);
                return true;
            }
        };

        match reconcile_service_startup().await {
            Ok(()) => {
                if let Err(error) = restore_desired_state().await {
                    tracing::warn!(
                        "Desired state restoration failed; keeping IPC available for GUI recovery: {:#}",
                        error
                    );
                }
            }
            Err(error) => tracing::warn!(
                "Service startup reconciliation failed; core starts remain blocked while IPC is available: {}",
                error
            ),
        }

        let result = run_ipc_supervisor_until_shutdown(async {
            let _ = shutdown_rx.recv().await;
        })
        .await;
        if let Err(error) = result {
            tracing::warn!("IPC supervisor failed: {}", error);
            drop(owner_guard);
            return true;
        }

        drop(owner_guard);
        false
    });

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
    info!("Current process PID: {}", pid);

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
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");

        tokio::select! {
            _ = sigint.recv() => info!("Received SIGINT (Ctrl+C)"),
            _ = sigterm.recv() => info!("Received SIGTERM"),
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

        let mut ctrl_c = ctrl_c().expect("Failed to install Ctrl+C handler");
        let mut ctrl_break = ctrl_break().expect("Failed to install Ctrl+Break handler");
        let mut ctrl_close = ctrl_close().expect("Failed to install Ctrl+Close handler");
        let mut ctrl_logoff = ctrl_logoff().expect("Failed to install Ctrl+Logoff handler");
        let mut ctrl_shutdown = ctrl_shutdown().expect("Failed to install Ctrl+Shutdown handler");

        tokio::select! {
            _ = ctrl_c.recv() => info!("Received Ctrl+C"),
            _ = ctrl_break.recv() => info!("Received Ctrl+Break"),
            _ = ctrl_close.recv() => info!("Received console close"),
            _ = ctrl_logoff.recv() => info!("Received logoff"),
            _ = ctrl_shutdown.recv() => info!("Received system shutdown"),
        }
    }
}

/// Packaging can inspect the binary without entering SCM or starting an IPC daemon.
fn print_version_if_requested() -> bool {
    if std::env::args_os().skip(1).any(|argument| argument == "--version") {
        println!("zenclash-service {}", zenclash_service::VERSION);
        true
    } else {
        false
    }
}
