// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "client", feature = "test"))]

mod common;

use anyhow::{Context as _, Result};
use common::{start_server, stop_server};
use serial_test::serial;
use std::path::{Path, PathBuf};
use zenclash_service::{
    OwnerCredentials, OwnerSessionProof, RuntimeAsset, RuntimeBundle, StageRejection, StageRuntimeOutcome,
    StartClashRequest, get_status, service_paths, stage_runtime, start_clash, stop_clash, test_owner_credentials,
};

fn bundle(app_root: &Path, yaml: &str) -> RuntimeBundle {
    RuntimeBundle {
        yaml: yaml.to_owned(),
        assets: Vec::new(),
        remote_providers: Vec::new(),
        core_path: app_root
            .join(format!("mock_binary{}", std::env::consts::EXE_SUFFIX))
            .to_string_lossy()
            .into_owned(),
    }
}

struct RunningCore {
    credentials: OwnerCredentials,
    session: OwnerSessionProof,
    app_root: PathBuf,
    generation: PathBuf,
    pid: u32,
}

impl RunningCore {
    async fn start(label: &str) -> Result<Self> {
        let app_root = std::env::temp_dir().join(format!("service-ipc-stage-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&app_root);
        std::fs::create_dir_all(&app_root)?;
        let app_root = std::fs::canonicalize(app_root)?;
        std::fs::copy(
            common::test_bin_path("mock_binary"),
            app_root.join(format!("mock_binary{}", std::env::consts::EXE_SUFFIX)),
        )?;
        let credentials = test_owner_credentials(&app_root)?;
        let token = "ab".repeat(32);
        let response = start_clash(
            &credentials,
            &StartClashRequest {
                runtime: bundle(&app_root, "mode: rule\n"),
                proposed_session_token: token.clone(),
                macos_proxy: None,
            },
        )
        .await?;
        anyhow::ensure!(response.code == 0, "{}", response.message);
        let generation = response.data.context("start omitted its result")?.session.generation;
        let pid = get_status(&credentials)
            .await?
            .data
            .context("status omitted data")?
            .core_pid
            .context("started core has no pid")?;
        let runtime_dir = service_paths()?.for_owner(&credentials.identity).runtime_dir();

        Ok(Self {
            credentials,
            session: OwnerSessionProof { generation, token },
            app_root,
            generation: runtime_dir,
            pid,
        })
    }

    async fn stage(&self, runtime: &RuntimeBundle) -> Result<StageRuntimeOutcome> {
        let response = stage_runtime(&self.credentials, &self.session, runtime).await?;
        anyhow::ensure!(response.code == 0, "{}", response.message);
        response.data.context("staging omitted its outcome")
    }

    async fn shut_down(self) -> Result<()> {
        let response = stop_clash(&self.credentials, &self.session).await?;
        anyhow::ensure!(response.code == 0, "{}", response.message);
        std::fs::remove_dir_all(self.generation)?;
        std::fs::remove_dir_all(self.app_root)?;
        Ok(())
    }
}

async fn with_server<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let server = start_server().await?;
    let result = test().await;
    stop_server(server).await?;
    result
}

#[tokio::test]
#[serial]
async fn staging_updates_configuration_without_restarting_the_core() -> Result<()> {
    with_server(|| async {
        let core = RunningCore::start("config").await?;
        let outcome = core.stage(&bundle(&core.app_root, "mode: global\n")).await?;

        assert!(matches!(outcome, StageRuntimeOutcome::Staged { .. }));
        assert_eq!(
            std::fs::read_to_string(core.generation.join("config.yaml"))?,
            "mode: global\n"
        );
        let status = get_status(&core.credentials)
            .await?
            .data
            .context("status omitted data")?;
        assert_eq!(status.core_pid, Some(core.pid));
        assert_eq!(status.active_generation, Some(core.session.generation));

        core.shut_down().await
    })
    .await
}

#[tokio::test]
#[serial]
async fn staging_updates_declared_assets_but_preserves_core_owned_files() -> Result<()> {
    with_server(|| async {
        let core = RunningCore::start("assets").await?;
        let source = core.app_root.join("provider.yaml");
        std::fs::write(&source, b"proxies: []\n")?;
        let core_state = core.generation.join("cache.db");
        std::fs::write(&core_state, b"selected node")?;

        let mut declared = bundle(&core.app_root, "mode: rule\n");
        declared.assets.push(RuntimeAsset {
            source: source.to_string_lossy().into_owned(),
            destination: "providers/copied.yaml".to_owned(),
        });
        core.stage(&declared).await?;
        assert_eq!(
            std::fs::read(core.generation.join("providers/copied.yaml"))?,
            b"proxies: []\n"
        );

        core.stage(&bundle(&core.app_root, "mode: direct\n")).await?;
        assert!(!core.generation.join("providers/copied.yaml").exists());
        assert_eq!(std::fs::read(&core_state)?, b"selected node");

        core.shut_down().await
    })
    .await
}

#[tokio::test]
#[serial]
async fn staging_declines_a_core_binary_change_without_touching_configuration() -> Result<()> {
    with_server(|| async {
        let core = RunningCore::start("core-path").await?;
        let other_core = core.app_root.join("other-core");
        std::fs::copy(common::test_bin_path("mock_binary"), &other_core)?;
        let mut changed = bundle(&core.app_root, "mode: global\n");
        changed.core_path = other_core.to_string_lossy().into_owned();

        assert_eq!(
            core.stage(&changed).await?,
            StageRuntimeOutcome::RestartRequired {
                reason: StageRejection::CorePathChanged,
            }
        );
        assert_eq!(
            std::fs::read_to_string(core.generation.join("config.yaml"))?,
            "mode: rule\n"
        );

        core.shut_down().await
    })
    .await
}
