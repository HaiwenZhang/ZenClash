// ZenClash native session safety regression, 2026-10-04. GPL-3.0-only.
use super::*;
use std::{sync::Arc, time::Duration};

async fn native_start(
    session: &ServiceSession,
    request: StartClashRequest,
) -> Result<StartClashResult, ServiceCallError> {
    let response = zenclash_service::start_clash(&session.credentials, &request).await?;
    check_response(response.code, response.message)?;
    response
        .data
        .ok_or(ServiceCallError::MissingReply("owner session information"))
}

#[tokio::test]
async fn unanswered_and_cancelled_start_keep_native_stop_authority_without_adopting_replacements()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("zenclash-pending-start-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    // Elevated Windows runners otherwise inherit Administrators as the owner,
    // which native authentication correctly refuses for the caller's SID.
    #[cfg(windows)]
    crate::service::repair_app_data_root_owner(&root)?;
    let executable = std::env::current_exe()?;
    let target = executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| std::io::Error::other("test target directory missing"))?;
    let core = target.join(format!(
        "zenclash-core-integration-test-core{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        core.is_file(),
        "build the ipc-tests mock core before this native test: {}",
        core.display()
    );
    let runtime = RuntimeBundle {
        yaml: "mode: rule\n".into(),
        assets: Vec::new(),
        remote_providers: Vec::new(),
        core_path: core.to_string_lossy().into_owned(),
    };
    let server = zenclash_service::run_ipc_server().await?;
    // Unix binds the socket when the spawned server task first runs.
    tokio::time::timeout(Duration::from_secs(5), async {
        while zenclash_service::get_version().await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let session = Arc::new(ServiceSession::connect(&root).await?);
    let replacement = ServiceSession::connect(&root).await?;
    let workflow = async {
        // A parsed successful native response is deliberately discarded before app acknowledgement.
        let lost = session.start_exchange(runtime.clone(), |request| async {
            native_start(&session, request).await?;
            Err(ServiceCallError::Transport(anyhow::anyhow!("lost start acknowledgement")))
        }).await;
        let lost = lost.unwrap_err();
        assert!(lost.mutation_result_unknown(), "unexpected start refusal: {lost:?}");
        assert!(session.status().await?.core_pid.is_some());
        assert!(matches!(session.active_proof(), Err(ServiceCallError::StartUnconfirmed)));
        assert!(session.snapshot().is_none());
        assert!(matches!(session.start(runtime.clone()).await, Err(ServiceCallError::StartUnconfirmed)));
        session.stop().await?;
        assert!(!session.status().await?.is_active);

        // Cancellation after native Start commits must preserve the same shutdown authority.
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let task_session = session.clone();
        let task_entered = entered.clone();
        let task_resume = resume.clone();
        let candidate = runtime.clone();
        let task = tokio::spawn(async move {
            task_session.start_exchange(candidate, |request| async {
                let result = native_start(&task_session, request).await?;
                task_entered.notify_one();
                task_resume.notified().await;
                Ok(result)
            }).await
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified()).await?;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(matches!(session.active_proof(), Err(ServiceCallError::StartUnconfirmed)));
        session.stop().await?;
        assert!(!session.status().await?.is_active);

        // A definite source refusal happens before native Stop and must retain the previous proof.
        session.start(runtime.clone()).await?;
        let previous = session.active_proof()?;
        let mut invalid = runtime.clone();
        invalid.core_path = root.join("missing-core").to_string_lossy().into_owned();
        assert!(session.start(invalid).await.is_err());
        assert_eq!(session.active_proof()?, previous);
        assert!(session.owns_status(&session.status().await?));
        session.stop().await?;

        // A later native generation with another proposed token must not be stopped by an old one.
        session.start_exchange(runtime.clone(), |request| async {
            native_start(&session, request).await?;
            Err(ServiceCallError::Transport(anyhow::anyhow!("lost start acknowledgement")))
        }).await.unwrap_err();
        replacement.start(runtime.clone()).await?;
        let proof = replacement.active_proof()?;
        let before = replacement.status().await?;
        assert!(matches!(session.stop().await,
            Err(ServiceCallError::Rejected {code, ..}) if code == ServiceErrorCode::StaleOwnerSession as u16));
        let after = replacement.status().await?;
        assert_eq!(after.core_pid, before.core_pid);
        assert_eq!(replacement.active_proof()?, proof);
        assert!(replacement.owns_status(&after));
        assert!(matches!(session.active_proof(), Err(ServiceCallError::StartUnconfirmed)));
        replacement.stop().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }.await;
    let _ = session.stop().await;
    let cleanup = replacement.stop().await;
    zenclash_service::stop_ipc_server().await?;
    server.await??;
    cleanup?;
    std::fs::remove_dir_all(root)?;
    workflow
}
