// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
//! IPC server lifecycle, listener supervision, owner-lease expiration, and on-demand idle exit.

mod handlers;
mod routes;

#[cfg(test)]
mod tests;

use self::handlers::{ADMISSION_CLOSED, OWNER_LIFECYCLE_LOCK};
use self::routes::create_ipc_router;
use crate::core::desired::{clear_active_owner, load_active_owner, persist_owner_core_stopped_by_key};
use crate::core::logger::flush_writer;
use crate::core::manager::CORE_MANAGER;
use crate::core::paths::service_paths;
use crate::core::state::set_service_lifecycle_state;
use crate::core::structure::ServiceLifecycleState;
use anyhow::{Result as AnyResult, anyhow};
use kode_bridge::{IpcHttpServer, Result, ServerConfig};
use std::{
    future::Future,
    sync::{LazyLock, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const IPC_MAX_RESTARTS: u32 = 10;
const IPC_RESTART_WINDOW: Duration = Duration::from_secs(10);
const IPC_MAX_BACKOFF: Duration = Duration::from_millis(500);
pub(super) const IPC_HANDLER_TIMEOUT: Duration = Duration::from_secs(25);
#[cfg(any(test, all(windows, not(feature = "test"))))]
pub(super) const WINDOWS_CONTROL_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012019b;;;AU)";
#[cfg(all(windows, feature = "test"))]
const WINDOWS_TEST_CONTROL_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)";

// Serializes listener replacement so old cleanup cannot remove the new socket.
static IPC_LIFECYCLE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Listener state, replaced independently while `IPC_LIFECYCLE_LOCK` serializes supervisors.
static IPC_SERVER: LazyLock<Mutex<Option<IpcHttpServer>>> = LazyLock::new(|| Mutex::new(None));
static IPC_SHUTDOWN_SENDER: LazyLock<Mutex<Option<oneshot::Sender<()>>>> = LazyLock::new(|| Mutex::new(None));
static IPC_SHUTDOWN_DONE: LazyLock<Mutex<Option<oneshot::Receiver<()>>>> = LazyLock::new(|| Mutex::new(None));

/// Shuts down the listener before dropping its handle.
async fn shutdown_ipc_server() {
    let mut guard = IPC_SERVER.lock().await;
    if let Some(server) = guard.as_mut() {
        server.shutdown();
    }
    *guard = None;
}

///
/// # Errors
/// Returns native listener, registration or owner-preparation errors; callers must handle any startup already performed.
pub async fn run_ipc_server() -> Result<JoinHandle<Result<()>>> {
    let _lifecycle_guard = IPC_LIFECYCLE_LOCK.lock().await;

    make_ipc_dir().await?;
    cleanup_stale_ipc_socket().await?;
    init_ipc_state().await?;

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel::<()>();

    *IPC_SHUTDOWN_SENDER.lock().await = Some(shutdown_tx);
    *IPC_SHUTDOWN_DONE.lock().await = Some(done_rx);

    if let Some(mut server) = IPC_SERVER.lock().await.take() {
        {
            let _owner_guard = OWNER_LIFECYCLE_LOCK.lock().await;
            super::owner_lease::initialize(
                load_active_owner()
                    .await
                    .map_err(|error| kode_bridge::KodeBridgeError::custom(error.to_string()))?
                    .map(|owner| owner.generation),
            );
        }
        let handle = tokio::spawn(async move {
            let res = tokio::select! {
                res = server.serve() => res,
                _ = &mut shutdown_rx => Ok(()),
                _ = supervise_owner_lease() => Ok(()),
            };

            let _ = done_tx.send(());
            res
        });
        Ok(handle)
    } else {
        Err(kode_bridge::KodeBridgeError::configuration(
            "IPC server not initialized".to_string(),
        ))
    }
}

async fn supervise_owner_lease() {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(error) = retire_expired_owner().await {
            warn!("Failed to retire expired GUI ownership; cleanup will retry: {error:#}");
        }
    }
}

async fn retire_expired_owner() -> AnyResult<()> {
    let _guard = OWNER_LIFECYCLE_LOCK.lock().await;
    let Some(active) = load_active_owner().await? else {
        return Ok(());
    };
    if !super::owner_lease::is_expired(active.generation) {
        return Ok(());
    }
    info!(
        generation = active.generation,
        "GUI ownership expired; stopping its core"
    );
    CORE_MANAGER.lock().await.stop_core().await?;
    persist_owner_core_stopped_by_key(&active.owner_key).await?;
    clear_active_owner().await?;
    super::owner_lease::reset(None);
    Ok(())
}

///
/// # Errors
/// Returns native listener, registration or owner-preparation errors; callers must handle any startup already performed.
pub async fn stop_ipc_server() -> Result<()> {
    let _lifecycle_guard = IPC_LIFECYCLE_LOCK.lock().await;

    CORE_MANAGER
        .lock()
        .await
        .stop_core()
        .await
        .map_err(|error| kode_bridge::KodeBridgeError::custom(error.to_string()))?;

    if let Some(sender) = IPC_SHUTDOWN_SENDER.lock().await.take() {
        let _ = sender.send(());
    }

    if let Some(done) = IPC_SHUTDOWN_DONE.lock().await.take() {
        let _ = done.await;
    }

    shutdown_ipc_server().await;

    cleanup_ipc_path().await?;
    #[cfg(windows)]
    tokio::time::sleep(std::time::Duration::from_millis(70)).await;

    Ok(())
}

///
/// # Errors
/// Returns native listener, registration or owner-preparation errors; callers must handle any startup already performed.
pub async fn run_ipc_supervisor_until_shutdown(shutdown: impl Future<Output = ()>) -> AnyResult<()> {
    let result = run_supervisor(shutdown).await;
    // Do not rely on the periodic flusher at process exit.
    flush_writer();
    result
}

async fn run_supervisor(shutdown: impl Future<Output = ()>) -> AnyResult<()> {
    ADMISSION_CLOSED.store(false, Ordering::Release);
    set_service_lifecycle_state(ServiceLifecycleState::Starting);
    info!("Starting IPC server...");

    let mut server_handle = match run_ipc_server().await {
        Ok(handle) => handle,
        Err(error) => {
            set_service_lifecycle_state(ServiceLifecycleState::Fatal);
            return Err(anyhow!("failed to start IPC server: {}", error));
        }
    };
    set_service_lifecycle_state(ServiceLifecycleState::Running);
    info!("IPC server started successfully. Waiting for shutdown signal...");

    #[cfg(all(target_os = "macos", not(feature = "test")))]
    let activation = crate::activation::macos::ActivationListener::new(crate::MACOS_SERVICE_ID)?;
    #[cfg(all(windows, not(feature = "test")))]
    let activation = match crate::activation::windows::registered_service_can_activate() {
        Ok(true) => Some(crate::activation::windows::ActivationListener::new()?),
        Ok(false) => None,
        Err(error) => {
            warn!("Cannot verify Windows pipe activation; keeping the service resident: {error:#}");
            None
        }
    };
    #[cfg(all(windows, not(feature = "test")))]
    let idle_failure = parking_lot::Mutex::new(None);
    let idle = async {
        #[cfg(all(target_os = "macos", not(feature = "test")))]
        wait_for_macos_idle(&activation).await;
        #[cfg(all(windows, not(feature = "test")))]
        if let Some(activation) = activation.as_ref() {
            if let Err(error) = wait_for_windows_idle(activation).await {
                crate::windows_service_begin_stopping();
                *idle_failure.lock() = Some(error);
            }
        } else {
            std::future::pending::<()>().await;
        }
        #[cfg(any(all(not(target_os = "macos"), not(windows)), feature = "test"))]
        std::future::pending::<()>().await;
    };
    tokio::pin!(idle);

    let mut restart_timestamps: Vec<Instant> = Vec::new();
    let mut consecutive_attempt = 0u32;
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut idle => {
                info!("No GUI clients or core owner remain; exiting the on-demand service");
                break;
            }
            _ = &mut shutdown => {
                info!("Shutdown signal received. Stopping IPC server...");
                break;
            }
            join_result = &mut server_handle => {
                let reason = match join_result {
                    Ok(Ok(())) => "IPC server exited cleanly".to_string(),
                    Ok(Err(error)) => format!("IPC server returned error: {error}"),
                    Err(error) => format!("IPC server task failed: {error}"),
                };
                warn!("{reason}; rebuilding IPC listener in-process");
                set_service_lifecycle_state(ServiceLifecycleState::RecoveringIpc);

                let now = Instant::now();
                restart_timestamps.retain(|t| now.duration_since(*t) < IPC_RESTART_WINDOW);
                if restart_timestamps.is_empty() {
                    consecutive_attempt = 0;
                }
                restart_timestamps.push(now);

                if restart_timestamps.len() as u32 > IPC_MAX_RESTARTS {
                    set_service_lifecycle_state(ServiceLifecycleState::Fatal);
                    return Err(anyhow!(
                        "IPC server restarted {} times in {}s",
                        restart_timestamps.len(),
                        IPC_RESTART_WINDOW.as_secs()
                    ));
                }

                let delay = ipc_backoff_delay(consecutive_attempt);
                consecutive_attempt += 1;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }

                server_handle = match run_ipc_server().await {
                    Ok(handle) => handle,
                    Err(error) => {
                        set_service_lifecycle_state(ServiceLifecycleState::Fatal);
                        return Err(anyhow!("failed to rebuild IPC server: {}", error));
                    }
                };
                set_service_lifecycle_state(ServiceLifecycleState::Running);
                info!("IPC listener rebuilt successfully");
            }
        }
    }

    stop_ipc_server().await?;
    server_handle.abort();
    #[cfg(all(windows, not(feature = "test")))]
    if let Some(error) = idle_failure.lock().take() {
        set_service_lifecycle_state(ServiceLifecycleState::Fatal);
        return Err(error);
    }
    Ok(())
}

#[cfg(all(windows, not(feature = "test")))]
async fn wait_for_windows_idle(activation: &crate::activation::windows::ActivationListener) -> AnyResult<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if activation.is_failed() {
            return Err(anyhow!("Windows service activation listener exited unexpectedly"));
        }
        let _owner = OWNER_LIFECYCLE_LOCK.lock().await;
        let active = match load_active_owner().await {
            Ok(owner) => owner.is_some(),
            Err(error) => {
                activation.prepare_idle_exit(true);
                warn!("Cannot determine Windows service idleness: {error:#}");
                continue;
            }
        };
        let core_running = CORE_MANAGER.lock().await.status().await.core_pid.is_some();
        if activation.prepare_idle_exit(active || core_running) {
            ADMISSION_CLOSED.store(true, Ordering::Release);
            return Ok(());
        }
    }
}

#[cfg(all(target_os = "macos", not(feature = "test")))]
async fn wait_for_macos_idle(activation: &crate::activation::macos::ActivationListener) {
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    loop {
        interval.tick().await;
        let _owner = OWNER_LIFECYCLE_LOCK.lock().await;
        let active = match load_active_owner().await {
            Ok(owner) => owner.is_some(),
            // Unreadable ownership must be recovered, never assumed empty.
            Err(error) => {
                warn!("Cannot determine service idleness: {error:#}");
                continue;
            }
        };
        let core_running = CORE_MANAGER.lock().await.status().await.core_pid.is_some();
        if activation.prepare_idle_exit(active || core_running) {
            ADMISSION_CLOSED.store(true, Ordering::Release);
            return;
        }
    }
}

fn ipc_backoff_delay(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }

    Duration::from_millis(100u64 << (attempt - 1).min(3)).min(IPC_MAX_BACKOFF)
}

async fn make_ipc_dir() -> Result<()> {
    #[cfg(unix)]
    {
        let paths = service_paths()?;
        let Some(dir_path) = paths.ipc_path().parent() else {
            return Ok(());
        };

        ensure_control_runtime_dir(dir_path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_control_runtime_dir(dir: &std::path::Path) -> std::io::Result<()> {
    crate::core::unix_security::ensure_service_directory(dir, 0o755)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

async fn cleanup_ipc_path() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::fs;

        let paths = service_paths()?;
        if paths.ipc_path().exists() {
            fs::remove_file(paths.ipc_path()).await?;
        }
    }
    Ok(())
}

async fn cleanup_stale_ipc_socket() -> Result<()> {
    #[cfg(unix)]
    {
        let paths = service_paths()?;
        let socket_path = paths.ipc_path();
        if !socket_path.exists() {
            return Ok(());
        }

        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::UnixStream::connect(socket_path),
        )
        .await
        {
            Ok(Ok(_stream)) => {
                warn!("IPC socket {:?} is reachable; leaving it in place", socket_path);
            }
            _ => {
                info!("Cleaning up stale IPC socket: {:?}", socket_path);
                tokio::fs::remove_file(socket_path).await?;
            }
        }
    }
    #[cfg(windows)]
    {}
    Ok(())
}

async fn init_ipc_state() -> Result<()> {
    let server = create_ipc_server()?;
    let router = create_ipc_router()?;
    let server = server.router(router);
    *IPC_SERVER.lock().await = Some(server);
    Ok(())
}

fn create_ipc_server() -> Result<IpcHttpServer> {
    let paths = service_paths()?;

    let server = IpcHttpServer::with_config(
        paths.ipc_path(),
        ServerConfig {
            write_timeout: IPC_HANDLER_TIMEOUT,
            ..ServerConfig::default()
        },
    )?;

    #[cfg(unix)]
    {
        use platform_lib::{S_IRGRP, S_IROTH, S_IRUSR, S_IWGRP, S_IWOTH, S_IWUSR, mode_t};

        // Trust model: docs/development/service-security-model.md. The daemon
        // runs as root while the GUI runs as an ordinary user, so the socket
        // must be reachable by the desktop session. Authorization is per request
        // (kernel peer credentials plus the caller's owned app-data root), not
        // socket ownership. Windows restricts its control pipe to authenticated
        // users with an explicit SDDL for the same reason.
        let mode: mode_t = platform_lib::mode_t::from(S_IRUSR | S_IWUSR | S_IRGRP | S_IWGRP | S_IROTH | S_IWOTH);
        let server = server.with_listener_mode(mode);
        Ok(server)
    }

    #[cfg(windows)]
    {
        // Tests need create-instance access normally held by the production SYSTEM account.
        #[cfg(feature = "test")]
        let descriptor = WINDOWS_TEST_CONTROL_PIPE_SDDL;
        #[cfg(not(feature = "test"))]
        let descriptor = WINDOWS_CONTROL_PIPE_SDDL;
        let server = server.with_listener_security_descriptor(descriptor);
        Ok(server)
    }
}
