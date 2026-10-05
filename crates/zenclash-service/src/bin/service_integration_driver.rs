// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(feature = "client")]

use anyhow::Context as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::time::sleep;
#[cfg(feature = "test")]
use zenclash_service::test_owner_credentials;
use zenclash_service::{
    IpcConfig, MIN_REQUIRED_SERVICE_REVISION, OwnerSessionProof, ProtocolVersion, RuntimeBundle, StartClashRequest,
    get_status, get_version, set_config, start_clash, stop_clash,
};
#[cfg(not(feature = "test"))]
use zenclash_service::{OwnerCredentials, OwnerIdentity};

const IPC_READY_TIMEOUT: Duration = Duration::from_secs(20);
const IPC_PROBE_INTERVAL: Duration = Duration::from_millis(250);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: service-integration-driver <probe|ready|ping|start|stop>");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "probe" => probe_protocol().await?,
        "ready" => wait_protocol_ready().await?,
        "ping" => wait_ipc_ready().await?,
        "start" => start_flow().await?,
        "stop" => stop_flow().await?,
        _ => {
            eprintln!("usage: service-integration-driver <probe|ready|ping|start|stop>");
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn probe_protocol() -> anyhow::Result<()> {
    set_config(Some(IpcConfig {
        default_timeout: Duration::from_millis(250),
        max_retries: 1,
        retry_delay: Duration::from_millis(25),
    }))
    .await;
    let result = async {
        let response = get_version().await?;
        let info = response
            .data
            .ok_or_else(|| anyhow::anyhow!("service omitted protocol information"))?;
        if response.code != 0 || !info.supports_client(ProtocolVersion::current(), MIN_REQUIRED_SERVICE_REVISION) {
            anyhow::bail!("service protocol is not compatible");
        }
        Ok(())
    }
    .await;
    set_config(None).await;
    result
}

async fn wait_protocol_ready() -> anyhow::Result<()> {
    set_config(Some(IpcConfig {
        default_timeout: Duration::from_millis(250),
        max_retries: 1,
        retry_delay: Duration::from_millis(25),
    }))
    .await;

    let result: anyhow::Result<()> = async {
        let deadline = Instant::now() + IPC_READY_TIMEOUT;
        let mut last_error = None;
        while Instant::now() < deadline {
            match probe_protocol().await {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
            sleep(IPC_PROBE_INTERVAL).await;
        }
        if let Some(error) = last_error {
            anyhow::bail!(
                "service protocol did not become ready within {IPC_READY_TIMEOUT:?}; last failure: {error:#}"
            );
        }
        anyhow::bail!("service protocol did not become ready within {IPC_READY_TIMEOUT:?}");
    }
    .await;

    set_config(None).await;
    result
}

async fn start_flow() -> anyhow::Result<()> {
    wait_ipc_ready().await?;
    let config = RuntimeBundle {
        yaml: "mode: rule\n".to_string(),
        assets: vec![],
        remote_providers: Vec::new(),
        core_path: mock_binary_path()?,
    };
    let response = start_clash(
        &owner_credentials()?,
        &StartClashRequest {
            runtime: config,
            proposed_session_token: session_token()?,
            macos_proxy: None,
        },
    )
    .await?;
    if response.code != 0 {
        anyhow::bail!("service rejected Start: {} ({})", response.message, response.code);
    }
    let generation = response
        .data
        .ok_or_else(|| anyhow::anyhow!("service Start response omitted session"))?
        .session
        .generation;
    println!("{generation}");
    Ok(())
}

async fn stop_flow() -> anyhow::Result<()> {
    let response = stop_clash(&owner_credentials()?, &session_proof()?).await?;
    if response.code != 0 {
        anyhow::bail!("service rejected Stop: {} ({})", response.message, response.code);
    }
    Ok(())
}

fn session_token() -> anyhow::Result<String> {
    std::env::var("ZENCLASH_TEST_SESSION_TOKEN").context("ZENCLASH_TEST_SESSION_TOKEN is required")
}

fn session_proof() -> anyhow::Result<OwnerSessionProof> {
    let generation =
        std::env::var("ZENCLASH_TEST_SESSION_GENERATION").context("ZENCLASH_TEST_SESSION_GENERATION is required")?;
    Ok(OwnerSessionProof {
        generation: generation
            .parse()
            .context("ZENCLASH_TEST_SESSION_GENERATION must be an unsigned integer")?,
        token: session_token()?,
    })
}

async fn wait_ipc_ready() -> anyhow::Result<()> {
    set_config(Some(IpcConfig {
        default_timeout: Duration::from_millis(250),
        max_retries: 1,
        retry_delay: Duration::from_millis(25),
    }))
    .await;

    let result: anyhow::Result<()> = async {
        let deadline = Instant::now() + IPC_READY_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(response) = get_status(&owner_credentials()?).await
                && response.code == 0
                && response.data.is_some()
            {
                return Ok(());
            }
            sleep(IPC_PROBE_INTERVAL).await;
        }
        anyhow::bail!("IPC server not reachable within {:?}", IPC_READY_TIMEOUT)
    }
    .await;

    set_config(None).await;
    result
}

#[cfg(feature = "test")]
fn owner_credentials() -> anyhow::Result<zenclash_service::OwnerCredentials> {
    test_owner_credentials(&std::env::current_dir()?)
}

#[cfg(not(feature = "test"))]
fn owner_credentials() -> anyhow::Result<OwnerCredentials> {
    let app_data_dir = std::env::current_dir()?;
    #[cfg(unix)]
    let identity = OwnerIdentity::Unix {
        uid: unsafe { platform_lib::geteuid() },
        gid: unsafe { platform_lib::getegid() },
    };
    #[cfg(windows)]
    let identity = OwnerIdentity::Windows {
        sid: std::env::var("ZENCLASH_TEST_OWNER_SID")?,
    };

    Ok(OwnerCredentials {
        identity,
        app_data_dir: app_data_dir.to_string_lossy().into_owned(),
        token: std::env::var("ZENCLASH_TEST_OWNER_TOKEN").ok(),
    })
}

fn mock_binary_path() -> anyhow::Result<String> {
    let current_exe = std::env::current_exe()?;
    let mut path = current_exe;
    path.pop();
    #[cfg(windows)]
    path.push("mock_binary.exe");
    #[cfg(not(windows))]
    path.push("mock_binary");
    if path.exists() {
        return Ok(path.to_string_lossy().to_string());
    }

    let status = Command::new("cargo")
        .args(["build", "--features", "test"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to build mock_binary");
    }
    if path.exists() {
        return Ok(path.to_string_lossy().to_string());
    }
    anyhow::bail!("mock_binary not found after build");
}
