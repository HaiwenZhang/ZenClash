// ZenClash integration regression, 2026-10-04. GPL-3.0-only; see NOTICE.md.
#![cfg(feature = "service-ipc-tests")]

use anyhow::{Context as _, Result};
use futures_util::StreamExt as _;
use zenclash_core::service::{RuntimeBundle, ServiceSession, StageRuntimeOutcome};

#[tokio::test]
async fn native_session_takeover_staging_and_stop_preserve_owner_proof() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("zenclash-integration-ipc-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    // Native service authentication requires the caller's SID even on elevated CI.
    #[cfg(windows)]
    zenclash_core::service::repair_app_data_root_owner(&root)?;
    let server = zenclash_service::run_ipc_server().await?;
    // Unix binds the socket when the spawned server task first runs.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while zenclash_service::get_version().await.is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await?;
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let first = ServiceSession::connect(&root).await?;
        let second = ServiceSession::connect(&root).await?;
        Ok::<_, anyhow::Error>((first, second))
    })
    .await??;
    let runtime = RuntimeBundle {
        yaml: "mode: rule\n".to_owned(),
        assets: Vec::new(),
        remote_providers: Vec::new(),
        core_path: env!("CARGO_BIN_EXE_zenclash-core-integration-test-core").to_owned(),
    };

    assert!(first.snapshot().is_none());
    assert!(second.snapshot().is_none());
    let workflow = async {
        let first_start = first.start(runtime.clone()).await?;
        let first_proof = first.active_proof()?;
        assert_eq!(first_start.session.generation, first_proof.generation);
        assert_eq!(first_proof.token.len(), 64);
        let status = first.status().await?;
        assert!(first.owns_status(&status));
        assert!(status.core_pid.is_some());
        assert_eq!(
            first.snapshot().and_then(|status| status.core_pid),
            status.core_pid
        );
        assert!(zenclash_core::service::reserve_sidecar().await.is_err());

        let version = first
            .controller_request(
                "GET",
                "/version",
                None,
                "",
                std::time::Duration::from_secs(2),
            )
            .await?;
        assert_eq!(version.status, 200);
        let value: serde_json::Value = serde_json::from_slice(&version.body)?;
        assert_eq!(value["version"], "integration-fixture");
        let mut traffic = first.controller_socket("/traffic", "").await?;
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), traffic.next())
            .await?
            .context("fixture traffic ended")??;
        let value: serde_json::Value = serde_json::from_str(&message.into_text()?)?;
        assert_eq!(value["down"], 2);
        drop(traffic);
        let mut logs = first.controller_socket("/logs?oversized", "").await?;
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), logs.next())
            .await?
            .context("fixture logs ended")?;
        assert!(matches!(
            message,
            Err(tokio_tungstenite::tungstenite::Error::Capacity(_))
        ));
        drop(logs);

        let mut changed = runtime.clone();
        changed.yaml = "mode: direct\n".to_owned();
        let staged = first.stage_runtime(&changed).await?;
        let StageRuntimeOutcome::Staged { config_path } = staged else {
            anyhow::bail!("fixture should allow staging");
        };
        let before = first
            .controller_request(
                "GET",
                "/configs",
                None,
                "",
                std::time::Duration::from_secs(2),
            )
            .await?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&before.body)?["mode"],
            "rule"
        );
        let reload = serde_json::json!({"path":config_path});
        let loaded = first
            .controller_request(
                "PUT",
                "/configs?force=true",
                Some(&reload),
                "",
                std::time::Duration::from_secs(2),
            )
            .await?;
        assert_eq!(loaded.status, 204);
        let after = first
            .controller_request(
                "GET",
                "/configs",
                None,
                "",
                std::time::Duration::from_secs(2),
            )
            .await?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&after.body)?["mode"],
            "direct"
        );
        assert_eq!(first.active_proof()?, first_proof);

        let replacement = second.start(changed).await?;
        assert!(replacement.session.generation > first_start.session.generation);
        let status = second.status().await?;
        assert!(second.owns_status(&status));
        assert!(!first.owns_status(&status));
        first.status().await?;
        assert!(
            first.snapshot().is_none(),
            "cached replacement PID belongs to a different proof"
        );
        assert_eq!(
            second.snapshot().and_then(|status| status.core_pid),
            status.core_pid
        );
        assert!(matches!(
            first
                .controller_request(
                    "GET",
                    "/version",
                    None,
                    "",
                    std::time::Duration::from_secs(2)
                )
                .await,
            Err(zenclash_core::service::ServiceCallError::OwnerLost)
        ));
        let mut owner_watch = zenclash_core::service::runstate::OwnerWatch::new();
        assert_eq!(
            owner_watch.observe(first.owner_sample().await),
            zenclash_core::service::runstate::OwnerStep::Recover(
                zenclash_core::service::runstate::OwnerRecoveryReason::Displaced
            )
        );
        assert_eq!(
            owner_watch.observe(second.owner_sample().await),
            zenclash_core::service::runstate::OwnerStep::Continue
        );

        // The native stale-session refusal is accepted as release of only the old handle.
        first.stop().await?;
        let status = second.status().await?;
        assert!(second.owns_status(&status));
        assert!(status.core_pid.is_some());
        second.stop().await?;
        assert!(first.snapshot().is_none());
        assert!(second.snapshot().is_none());
        assert!(!second.status().await?.is_active);
        drop(zenclash_core::service::reserve_sidecar().await?);
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let first_cleanup = first.stop().await;
    let second_cleanup = second.stop().await;
    zenclash_service::stop_ipc_server().await?;
    server.await??;
    first_cleanup.context("cleanup first session")?;
    second_cleanup.context("cleanup replacement session")?;
    std::fs::remove_dir_all(root)?;
    workflow
}
