// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "client", feature = "test"))]

mod common;

use anyhow::{Context as _, Result};
use common::{start_server, stop_server, wait_until};
use serial_test::serial;
use std::time::Duration;
use tokio::sync::oneshot;
use zenclash_service::{
    CoreWatchdogTestConfig, OwnerSessionProof, RuntimeBundle, ServiceLifecycleState, StartClashRequest, connect,
    get_status, run_ipc_supervisor_until_shutdown, service_lifecycle_state, set_core_watchdog_config_for_tests,
    start_clash, stop_clash, stop_ipc_server,
};

#[tokio::test]
#[serial]
async fn ipc_supervisor_restarts_a_stopped_listener() -> Result<()> {
    let _ = stop_ipc_server().await;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let supervisor = tokio::spawn(run_ipc_supervisor_until_shutdown(async {
        let _ = shutdown_rx.await;
    }));

    wait_until("IPC startup", async || {
        connect().await.is_ok() && service_lifecycle_state() == ServiceLifecycleState::Running
    })
    .await?;
    stop_ipc_server().await?;
    wait_until("IPC restart", async || connect().await.is_ok()).await?;

    let _ = shutdown_tx.send(());
    supervisor.await??;
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
#[serial]
async fn sidecar_probe_does_not_inherit_long_client_retries() -> Result<()> {
    let _ = stop_ipc_server().await;
    zenclash_service::set_config(Some(zenclash_service::IpcConfig {
        default_timeout: Duration::from_secs(1),
        max_retries: 20,
        retry_delay: Duration::from_millis(500),
    }))
    .await;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        zenclash_service::execution::check_sidecar_available(),
    )
    .await;
    zenclash_service::set_config(None).await;
    assert!(
        result.is_ok(),
        "Sidecar inspection inherited the client's long retry budget"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_healthy_service_owner_prevents_a_second_instance() -> Result<()> {
    let _ = stop_ipc_server().await;
    let owner = zenclash_service::acquire_service_owner()
        .await?
        .context("current process did not acquire the service owner lock")?;
    let server = start_server().await?;

    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(common::test_bin_path("owner_lock_holder")).status(),
    )
    .await
    .context("second owner did not exit")??;
    assert_eq!(status.code(), Some(2));
    assert!(connect().await.is_ok());

    stop_server(server).await?;
    drop(owner);
    Ok(())
}

#[tokio::test]
#[serial]
async fn core_watchdog_stops_a_bounded_crash_loop() -> Result<()> {
    struct ResetWatchdog;
    impl Drop for ResetWatchdog {
        fn drop(&mut self) {
            set_core_watchdog_config_for_tests(None);
        }
    }

    let _reset = ResetWatchdog;
    set_core_watchdog_config_for_tests(Some(CoreWatchdogTestConfig {
        max_restarts: 2,
        restart_window: Duration::from_secs(10),
        max_backoff: Duration::ZERO,
    }));
    let server = start_server().await?;
    let credentials = common::owner_credentials();
    let token = "41".repeat(32);
    let response = start_clash(
        &credentials,
        &StartClashRequest {
            runtime: RuntimeBundle {
                yaml: "mode: rule\n".to_owned(),
                assets: Vec::new(),
                remote_providers: Vec::new(),
                core_path: common::test_bin_path("crash_binary").to_string_lossy().into_owned(),
            },
            proposed_session_token: token.clone(),
            macos_proxy: None,
        },
    )
    .await?;
    anyhow::ensure!(response.code == 0, "{}", response.message);
    let session = OwnerSessionProof {
        generation: response.data.context("start omitted its result")?.session.generation,
        token,
    };

    wait_until("watchdog limit", async || {
        get_status(&credentials)
            .await
            .ok()
            .and_then(|response| response.data)
            .is_some_and(|status| status.restart_count >= 2 && status.core_pid.is_none())
    })
    .await?;
    assert!(
        get_status(&credentials)
            .await?
            .data
            .context("status omitted data")?
            .last_core_exit_reason
            .is_some()
    );

    assert_eq!(stop_clash(&credentials, &session).await?.code, 0);
    stop_server(server).await
}
