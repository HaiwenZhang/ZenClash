// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
// Each integration-test binary uses a different subset of these helpers.
#![allow(dead_code)]

use anyhow::Result;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zenclash_service::{OwnerCredentials, connect, run_ipc_server, stop_ipc_server, test_owner_credentials};

pub fn owner_credentials() -> OwnerCredentials {
    let app_data_dir = std::env::temp_dir().join(format!("service-ipc-owner-{}", std::process::id()));
    test_owner_credentials(&app_data_dir).expect("test owner credentials should be secured for the current user")
}

pub fn test_bin_path(name: &str) -> PathBuf {
    PathBuf::from(match name {
        "mock_binary" => env!("CARGO_BIN_EXE_mock_binary"),
        "crash_binary" => env!("CARGO_BIN_EXE_crash_binary"),
        "owner_lock_holder" => env!("CARGO_BIN_EXE_owner_lock_holder"),
        _ => panic!("unknown test binary: {name}"),
    })
}

pub async fn start_server() -> Result<tokio::task::JoinHandle<kode_bridge::Result<()>>> {
    let _ = stop_ipc_server().await;
    let server = run_ipc_server().await?;
    wait_until("IPC startup", async || connect().await.is_ok()).await?;
    Ok(server)
}

pub async fn stop_server(server: tokio::task::JoinHandle<kode_bridge::Result<()>>) -> Result<()> {
    stop_ipc_server().await?;
    server.await??;
    Ok(())
}

pub async fn wait_until(label: &str, mut condition: impl AsyncFnMut() -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for {label}")
}
