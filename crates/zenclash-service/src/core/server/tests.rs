use super::WINDOWS_CONTROL_PIPE_SDDL;
use super::handlers::{
    ADMISSION_CLOSED, OwnerLifecycleGate, OwnerTransition, enter_owner_lifecycle, require_active_owner,
    require_active_session, run_owner_transition, service_error,
};
use crate::ServiceErrorCode;
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::desired::{ActiveOwnerState, clear_active_owner, commit_active_owner_session, persist_active_owner};
use crate::{OwnerIdentity, OwnerSessionProof};
use http::StatusCode;
use serial_test::serial;

fn owner(uid: u32) -> AuthenticatedOwner {
    AuthenticatedOwner {
        key: uid.to_string(),
        identity: OwnerIdentity::Unix { uid, gid: 20 },
        app_data_root: std::env::temp_dir(),
    }
}

#[tokio::test]
#[serial]
async fn committed_idle_exit_rejects_a_new_authenticated_start() {
    struct RestoreAdmission;
    impl Drop for RestoreAdmission {
        fn drop(&mut self) {
            ADMISSION_CLOSED.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let _restore = RestoreAdmission;
    let owner = owner(96_003);
    assert!(matches!(
        enter_owner_lifecycle(&owner, OwnerLifecycleGate::Unchecked).await,
        std::ops::ControlFlow::Continue(_)
    ));
    ADMISSION_CLOSED.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        enter_owner_lifecycle(&owner, OwnerLifecycleGate::Unchecked).await,
        std::ops::ControlFlow::Break(_)
    ));
}

struct RecordingTransition {
    events: Vec<&'static str>,
    active_owner: ActiveOwnerState,
    running_pid: u32,
    next_owner: ActiveOwnerState,
    stop_fails: bool,
}

impl OwnerTransition for RecordingTransition {
    async fn stop_previous_core(&mut self) -> anyhow::Result<()> {
        self.events.push("stop_a");
        if self.stop_fails {
            anyhow::bail!("stop failed");
        }
        self.running_pid = 0;
        Ok(())
    }

    async fn start_new_core(&mut self) -> anyhow::Result<()> {
        self.events.push("start_b");
        self.running_pid = 202;
        Ok(())
    }

    async fn commit_new_owner(&mut self) -> anyhow::Result<ActiveOwnerState> {
        self.events.push("commit_b");
        self.active_owner = self.next_owner.clone();
        Ok(self.active_owner.clone())
    }
}

fn recording_transition() -> RecordingTransition {
    RecordingTransition {
        events: Vec::new(),
        active_owner: ActiveOwnerState::from(&owner(96_001)),
        running_pid: 101,
        next_owner: ActiveOwnerState::from(&owner(96_002)),
        stop_fails: false,
    }
}

#[tokio::test]
async fn owner_transition_successful_takeover_has_exact_order() -> anyhow::Result<()> {
    let mut transition = recording_transition();

    let active = run_owner_transition(&mut transition).await?;

    assert_eq!(transition.events, ["stop_a", "start_b", "commit_b"]);
    assert_eq!(active.owner_key, "96002");
    assert_eq!(transition.active_owner.owner_key, "96002");
    assert_eq!(transition.running_pid, 202);
    Ok(())
}

#[tokio::test]
async fn owner_transition_stop_failure_preserves_old_owner() {
    let mut transition = recording_transition();
    transition.stop_fails = true;

    let error = run_owner_transition(&mut transition)
        .await
        .expect_err("core stop failure must abort takeover");

    assert_eq!(error.code, ServiceErrorCode::OwnerSwitchFailed);
    assert_eq!(transition.events, ["stop_a"]);
    assert_eq!(transition.active_owner.owner_key, "96001");
    assert_eq!(transition.running_pid, 101);
}

#[test]
fn service_error_maps_forbidden_origin_and_session_conflict_http_statuses() -> anyhow::Result<()> {
    let forbidden = service_error(ServiceError::forbidden_origin("origin blocked"))?;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    let conflict = service_error(ServiceError::session_conflict("session mismatch"))?;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    Ok(())
}

#[test]
fn windows_control_pipe_allows_authenticated_users_not_everyone() {
    assert!(WINDOWS_CONTROL_PIPE_SDDL.contains(";;;AU)"));
    assert!(!WINDOWS_CONTROL_PIPE_SDDL.contains(";;;WD)"));
    assert!(!WINDOWS_CONTROL_PIPE_SDDL.contains("GRGW;;;AU"));
    assert!(WINDOWS_CONTROL_PIPE_SDDL.contains("0x0012019b;;;AU"));
    assert!(!WINDOWS_CONTROL_PIPE_SDDL.contains("0x0012019f;;;AU"));
}

#[tokio::test]
#[serial]
async fn non_active_owner_receives_stable_error() -> anyhow::Result<()> {
    let active = owner(92_001);
    let inactive = owner(92_002);
    commit_active_owner_session(&active, &"10".repeat(32)).await?;

    let error = require_active_owner(&inactive)
        .await
        .expect_err("non-active owner must be rejected");

    assert_eq!(error.code, ServiceErrorCode::NotActive);
    clear_active_owner().await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn same_owner_new_session_invalidates_old_proof() -> anyhow::Result<()> {
    clear_active_owner().await?;
    let owner = owner(95_001);
    let first = commit_active_owner_session(&owner, &"11".repeat(32)).await?;
    let first_proof = OwnerSessionProof {
        generation: first.generation,
        token: "11".repeat(32),
    };
    require_active_session(&owner, &first_proof).await?;
    let second = commit_active_owner_session(&owner, &"22".repeat(32)).await?;
    assert!(second.generation > first.generation);
    assert_eq!(
        require_active_session(&owner, &first_proof)
            .await
            .expect_err("old proof must be stale")
            .code,
        ServiceErrorCode::StaleOwnerSession,
    );
    clear_active_owner().await?;
    Ok(())
}

#[tokio::test]
#[serial]
async fn legacy_active_owner_session_fails_closed() -> anyhow::Result<()> {
    clear_active_owner().await?;
    let owner = owner(95_002);
    persist_active_owner(&owner).await?;
    let proof = OwnerSessionProof {
        generation: 0,
        token: "55".repeat(32),
    };

    assert_eq!(
        require_active_session(&owner, &proof)
            .await
            .expect_err("legacy owner must not authenticate a session")
            .code,
        ServiceErrorCode::StaleOwnerSession,
    );
    clear_active_owner().await?;
    Ok(())
}
