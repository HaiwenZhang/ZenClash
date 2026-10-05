// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
use crate::core::process::{process_identity, terminate_process_if_identity};
use crate::core::runtime::{
    cleanup_core_socket, is_core_socket_reachable, read_core_runtime_record, remove_core_runtime_record,
};
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

static STARTUP_RECONCILED: AtomicBool = AtomicBool::new(cfg!(feature = "test"));

pub(super) fn ensure_startup_reconciled() -> Result<()> {
    anyhow::ensure!(
        STARTUP_RECONCILED.load(Ordering::Acquire),
        "core startup reconciliation has not completed"
    );
    Ok(())
}

///
/// # Errors
/// Returns errors reading runtime evidence, identifying or terminating a verified prior process.
pub async fn reconcile_service_startup() -> Result<()> {
    STARTUP_RECONCILED.store(false, Ordering::Release);
    info!("Running service startup reconciliation");

    let Some(record) = read_core_runtime_record().await? else {
        STARTUP_RECONCILED.store(true, Ordering::Release);
        return Ok(());
    };

    let current_identity = process_identity(record.pid)?;
    let socket_reachable = is_core_socket_reachable(&record.ipc_path).await;

    if current_identity.as_ref() == Some(&record.identity) {
        warn!(
            "Found verified previous core process {} during startup; stopping it before supervision resumes",
            record.pid
        );
        terminate_process_if_identity(record.pid, &record.identity).await?;
        cleanup_core_socket(&record.ipc_path).await;
        remove_core_runtime_record().await;
        STARTUP_RECONCILED.store(true, Ordering::Release);
        return Ok(());
    }

    if let Some(current_identity) = current_identity {
        warn!(
            "Runtime PID {} now belongs to a different process ({:?}); refusing to terminate it",
            record.pid, current_identity
        );
    }
    if !socket_reachable {
        info!("Cleaning stale core socket from dead process: {}", record.ipc_path);
        cleanup_core_socket(&record.ipc_path).await;
    } else {
        warn!(
            "Core runtime PID {} is dead but socket {} is reachable; leaving socket untouched",
            record.pid, record.ipc_path
        );
    }

    remove_core_runtime_record().await;
    STARTUP_RECONCILED.store(true, Ordering::Release);
    Ok(())
}
