// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#![cfg(all(feature = "standalone", feature = "client", feature = "test"))]

mod common;

use anyhow::{Context as _, Result};
use common::{start_server, stop_server};
use serde::Deserialize;
use serial_test::serial;
use zenclash_service::{
    IpcCommand, OwnerCredentials, OwnerSessionProof, RuntimeBundle, ServiceErrorCode, StartClashRequest,
    StartClashResult, connect, get_status, start_clash, stop_clash,
};

#[derive(Deserialize)]
struct WireResponse {
    code: u16,
}

fn runtime_bundle() -> RuntimeBundle {
    RuntimeBundle {
        yaml: "mode: rule\n".to_owned(),
        assets: Vec::new(),
        remote_providers: Vec::new(),
        core_path: common::test_bin_path("mock_binary").to_string_lossy().into_owned(),
    }
}

async fn start(credentials: &OwnerCredentials, token: &str) -> Result<(StartClashResult, OwnerSessionProof)> {
    let response = start_clash(
        credentials,
        &StartClashRequest {
            runtime: runtime_bundle(),
            proposed_session_token: token.to_owned(),
            macos_proxy: None,
        },
    )
    .await?;
    anyhow::ensure!(response.code == 0, "{}", response.message);
    let result = response.data.context("start omitted its result")?;
    let session = OwnerSessionProof {
        generation: result.session.generation,
        token: token.to_owned(),
    };
    Ok((result, session))
}

#[tokio::test]
#[serial]
async fn protocol_mismatch_is_rejected_before_payload_deserialization() -> Result<()> {
    let server = start_server().await?;
    let response = connect()
        .await?
        .post(IpcCommand::StartClash.as_ref())
        .json_body(&serde_json::Value::String("invalid request".to_owned()))
        .send()
        .await?
        .json::<WireResponse>()?;

    assert_eq!(response.code, ServiceErrorCode::ProtocolMismatch as u16);
    stop_server(server).await
}

#[tokio::test]
#[serial]
async fn restarting_an_owner_invalidates_the_previous_session() -> Result<()> {
    let server = start_server().await?;
    let credentials = common::owner_credentials();

    let (first, first_session) = start(&credentials, &"11".repeat(32)).await?;
    let (second, second_session) = start(&credentials, &"22".repeat(32)).await?;

    assert!(second.session.generation > first.session.generation);
    assert_eq!(
        zenclash_service::heartbeat(&credentials, &first_session).await?.code,
        ServiceErrorCode::StaleOwnerSession as u16
    );
    assert_eq!(
        zenclash_service::heartbeat(&credentials, &second_session).await?.code,
        0
    );
    assert_eq!(
        stop_clash(&credentials, &first_session).await?.code,
        ServiceErrorCode::StaleOwnerSession as u16
    );
    assert_eq!(
        zenclash_service::set_tun_dns(&credentials, &first_session, true)
            .await?
            .code,
        ServiceErrorCode::StaleOwnerSession as u16
    );
    assert_eq!(
        zenclash_service::set_tun_dns(&credentials, &second_session, true)
            .await?
            .code,
        0
    );
    assert_eq!(
        zenclash_service::set_tun_dns(&credentials, &second_session, false)
            .await?
            .code,
        0
    );
    let status = get_status(&credentials).await?.data.context("status omitted data")?;
    assert!(status.is_active);
    assert!(status.core_pid.is_some());
    assert_eq!(stop_clash(&credentials, &second_session).await?.code, 0);

    stop_server(server).await
}

#[tokio::test]
#[serial]
async fn exited_gui_owner_does_not_leave_a_running_core() -> Result<()> {
    let server = start_server().await?;
    let credentials = common::owner_credentials();
    let token = "dd".repeat(32);
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_service-integration-driver"))
        .arg("start")
        .current_dir(&credentials.app_data_dir)
        .env("ZENCLASH_TEST_SESSION_TOKEN", &token)
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "isolated owner could not start its core");
    let generation = std::str::from_utf8(&output.stdout)?.trim().parse()?;
    let session = OwnerSessionProof { generation, token };
    let deadline =
        tokio::time::Instant::now() + zenclash_service::OWNER_SESSION_LEASE_TIMEOUT + std::time::Duration::from_secs(6);
    let mut stopped = false;
    while tokio::time::Instant::now() < deadline {
        let status = get_status(&credentials).await?.data.context("status omitted data")?;
        if !status.is_active && status.core_pid.is_none() && !status.desired_core_should_be_running {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !stopped {
        let _ = stop_clash(&credentials, &session).await;
    } else {
        common::wait_until("released core execution", async || {
            zenclash_service::inspect_installation(&[])
                .await
                .is_ok_and(|status| !status.core_busy)
        })
        .await?;
        let (_replacement, proof) = start(&credentials, &"ee".repeat(32)).await?;
        assert_eq!(stop_clash(&credentials, &proof).await?.code, 0);
    }
    stop_server(server).await?;
    assert!(
        stopped,
        "GUI exited but its service core and listener reservation stayed active"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_new_owner_takes_over_and_the_previous_owner_becomes_inactive() -> Result<()> {
    let server = start_server().await?;
    let root = std::env::temp_dir();
    let owner_a = zenclash_service::test_owner_credentials_for_uid(
        &root.join(format!("service-ipc-owner-a-{}", std::process::id())),
        91_001,
    )?;
    let owner_b = zenclash_service::test_owner_credentials_for_uid(
        &root.join(format!("service-ipc-owner-b-{}", std::process::id())),
        91_002,
    )?;

    let (_, session_a) = start(&owner_a, &"33".repeat(32)).await?;
    let (_, session_b) = start(&owner_b, &"44".repeat(32)).await?;

    assert!(!get_status(&owner_a).await?.data.context("no status")?.is_active);
    assert!(zenclash_service::inspect_installation(&[]).await?.core_busy);
    assert!(zenclash_service::execution::reserve_sidecar().await.is_err());
    assert!(get_status(&owner_b).await?.data.context("no status")?.is_active);
    assert_eq!(
        stop_clash(&owner_a, &session_a).await?.code,
        ServiceErrorCode::StaleOwnerSession as u16
    );
    assert_eq!(stop_clash(&owner_b, &session_b).await?.code, 0);

    stop_server(server).await
}

#[tokio::test]
#[serial]
async fn installation_query_reports_global_occupancy_and_guards_handoff() -> Result<()> {
    use zenclash_service::execution::reserve_sidecar;
    use zenclash_service::{CoreAvailability, CoreRequirement, inspect_installation};
    let server = start_server().await?;
    let name = format!("mihomo-alpha{}", std::env::consts::EXE_SUFFIX);
    let directory = zenclash_service::service_paths()?.core_dir();
    std::fs::create_dir_all(&directory)?;
    let core = directory.join(&name);
    let previous = std::fs::read(&core).ok();
    if core.exists() {
        std::fs::remove_file(&core)?;
    }
    let mut requirement = CoreRequirement { name, sha256: None };
    assert_eq!(
        inspect_installation(&[requirement.clone()]).await?.cores[0].availability,
        CoreAvailability::Missing
    );
    std::fs::write(&core, b"abc")?;
    requirement.sha256 = Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into());
    let status = inspect_installation(&[requirement.clone()]).await?;
    assert!(status.satisfies(&[requirement.clone()]));
    assert!(status.service_sha256.is_empty());
    requirement.sha256 = Some("00".repeat(32));
    let status = inspect_installation(&[requirement.clone()]).await?;
    assert_eq!(status.cores[0].availability, CoreAvailability::DigestMismatch);
    assert!(!status.satisfies(&[requirement]));
    if let Some(previous) = previous {
        std::fs::write(&core, previous)?;
    } else {
        std::fs::remove_file(&core)?;
    }

    let credentials = common::owner_credentials();
    let (_, session) = start(&credentials, &"aa".repeat(32)).await?;
    assert!(inspect_installation(&[]).await?.core_busy);
    assert!(reserve_sidecar().await.is_err());
    assert_eq!(stop_clash(&credentials, &session).await?.code, 0);
    let sidecar = reserve_sidecar().await?;
    assert!(inspect_installation(&[]).await?.core_busy);
    assert!(start(&credentials, &"bb".repeat(32)).await.is_err());
    drop(sidecar);
    let (_, session) = start(&credentials, &"cc".repeat(32)).await?;
    assert_eq!(stop_clash(&credentials, &session).await?.code, 0);
    stop_server(server).await
}
