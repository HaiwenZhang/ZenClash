//! Exercises GUI lifetime against the isolated native service and simulated core.
#![cfg(feature = "service-ipc-tests")]

use anyhow::{Context as _, Result, ensure};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zenclash_core::service::{RuntimeBundle, ServiceSession};

#[tokio::test]
async fn background_renewals_keep_the_core_alive_until_the_gui_session_is_dropped() -> Result<()> {
    let root = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
        "zenclash-native-lifetime-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    ));
    std::fs::create_dir_all(&root)?;
    #[cfg(windows)]
    zenclash_core::service::repair_app_data_root_owner(&root)?;
    let credentials = zenclash_core::service::owner_credentials(&root)?;
    let server = zenclash_service::run_ipc_server().await?;
    let workflow = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while zenclash_service::get_version().await.is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let owner = ServiceSession::connect(&root).await?;
        owner
            .start(RuntimeBundle {
                yaml: "mode: rule\ntun: {enable: false}\nrules: [MATCH,DIRECT]\n".into(),
                assets: Vec::new(),
                remote_providers: Vec::new(),
                core_path: env!("CARGO_BIN_EXE_zenclash-core-integration-test-core").into(),
            })
            .await?;
        let pid = owner
            .status()
            .await?
            .core_pid
            .context("core did not start")?;
        tokio::time::sleep(zenclash_service::OWNER_SESSION_LEASE_TIMEOUT + Duration::from_secs(1))
            .await;
        ensure!(
            owner.status().await?.core_pid == Some(pid),
            "a live background GUI lost its core"
        );
        drop(owner);
        tokio::time::timeout(
            zenclash_service::OWNER_SESSION_LEASE_TIMEOUT + Duration::from_secs(6),
            async {
                loop {
                    let status = zenclash_service::get_status(&credentials)
                        .await?
                        .data
                        .context("missing status")?;
                    if !status.is_active
                        && status.core_pid.is_none()
                        && !status.desired_core_should_be_running
                    {
                        break Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await
        .context("GUI session dropped but its core kept running")??;
        ensure!(
            !zenclash_service::inspect_installation(&[]).await?.core_busy,
            "execution reservation survived GUI exit"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup = zenclash_service::stop_ipc_server().await;
    let completion = server.await;
    std::fs::remove_dir_all(root)?;
    cleanup?;
    completion??;
    workflow
}
