use crate::core::auth::AuthenticatedOwner;
use crate::core::desired::{load_active_owner, load_owner_desired_state};
use crate::core::manager::CORE_MANAGER;
use crate::core::state::{core_lifecycle_state, service_lifecycle_state};
use crate::core::structure::{ServiceLifecycleState, ServiceStatusSnapshot};
use anyhow::Result;

pub async fn service_status_snapshot(owner: &AuthenticatedOwner) -> Result<ServiceStatusSnapshot> {
    let reported_service_state = service_lifecycle_state();
    let reported_core_state = core_lifecycle_state();
    let desired = load_owner_desired_state(&owner.key).await.unwrap_or_default();
    let active_owner = load_active_owner().await?;
    let active_generation = active_owner
        .as_ref()
        .filter(|active| active.owner_key == owner.key)
        .map(|active| active.generation);
    let is_active = active_generation.is_some();
    let core = if is_active {
        Some(CORE_MANAGER.lock().await.status().await)
    } else {
        None
    };

    let core_pid = core.as_ref().and_then(|core| core.core_pid);
    let service_state = effective_service_state(
        reported_service_state,
        reported_core_state,
        is_active,
        desired.core_should_be_running,
        core_pid,
    );

    Ok(ServiceStatusSnapshot {
        is_active,
        active_generation,
        service_state,
        core_pid,
        core_started_at: core.as_ref().and_then(|core| core.core_started_at),
        last_core_exit_reason: core.as_ref().and_then(|core| core.last_core_exit_reason.clone()),
        restart_count: core.as_ref().map_or(0, |core| core.restart_count),
        last_recovery_at: core.as_ref().and_then(|core| core.last_recovery_at),
        desired_core_should_be_running: desired.core_should_be_running,
        desired_generation: desired.generation,
        desired_updated_at: desired.updated_at,
    })
}

fn effective_service_state(
    reported: ServiceLifecycleState,
    core_reported: ServiceLifecycleState,
    is_active: bool,
    desired_running: bool,
    core_pid: Option<u32>,
) -> ServiceLifecycleState {
    if matches!(
        core_reported,
        ServiceLifecycleState::Fatal | ServiceLifecycleState::RecoveringCore | ServiceLifecycleState::Starting
    ) && is_active
        && desired_running
    {
        core_reported
    } else if reported != ServiceLifecycleState::Fatal && is_active && desired_running && core_pid.is_none() {
        ServiceLifecycleState::RecoveringCore
    } else {
        reported
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_service_state, service_status_snapshot};
    use crate::core::auth::AuthenticatedOwner;
    use crate::core::desired::{clear_active_owner, commit_active_owner_session};
    use crate::{OwnerIdentity, ServiceLifecycleState};
    use serial_test::serial;

    #[test]
    fn core_recovery_takes_precedence_over_ipc_running_state() {
        assert_eq!(
            effective_service_state(
                ServiceLifecycleState::Running,
                ServiceLifecycleState::Running,
                true,
                true,
                None,
            ),
            ServiceLifecycleState::RecoveringCore
        );
        assert_eq!(
            effective_service_state(
                ServiceLifecycleState::Fatal,
                ServiceLifecycleState::Running,
                true,
                true,
                None,
            ),
            ServiceLifecycleState::Fatal
        );
        assert_eq!(
            effective_service_state(
                ServiceLifecycleState::Running,
                ServiceLifecycleState::Fatal,
                true,
                true,
                None,
            ),
            ServiceLifecycleState::Fatal
        );
    }

    fn owner(uid: u32) -> AuthenticatedOwner {
        AuthenticatedOwner {
            key: uid.to_string(),
            identity: OwnerIdentity::Unix { uid, gid: 20 },
            app_data_root: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    #[serial]
    async fn inactive_owner_status_hides_active_core_details() -> anyhow::Result<()> {
        let active = owner(91_001);
        let inactive = owner(91_002);
        let active_session = commit_active_owner_session(&active, &"66".repeat(32)).await?;

        let status = service_status_snapshot(&inactive).await?;

        assert!(!status.is_active);
        assert_eq!(status.active_generation, None);
        assert_eq!(status.core_pid, None);
        assert_eq!(status.core_started_at, None);
        assert_eq!(status.last_core_exit_reason, None);
        assert_eq!(status.restart_count, 0);
        assert_eq!(status.last_recovery_at, None);
        assert_eq!(
            service_status_snapshot(&active).await?.active_generation,
            Some(active_session.generation)
        );
        clear_active_owner().await?;
        Ok(())
    }
}
