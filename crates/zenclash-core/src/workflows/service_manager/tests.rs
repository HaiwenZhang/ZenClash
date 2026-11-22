use super::*;
use crate::{CoreKind, MihomoClient, MihomoEndpoint};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

fn manager_for_session(session: CoreSession) -> ServiceManager {
    let capture = TrafficCaptureSession::new(
        session.clone(),
        crate::ControlledConfigStore::new(std::env::temp_dir().join("manager-command-fixture")),
        None,
        None,
    );
    ServiceManager::new(session, capture)
}

fn manager() -> ServiceManager {
    manager_for_session(
        CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::default()).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn an_external_controller_request_cannot_install_or_change_runtime_kind() {
    let manager = manager();
    let generation = manager.session.generation();
    assert!(matches!(
        manager.request_enable_tun(),
        Err(ServiceManagerError::Unsupported)
    ));
    assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    assert_eq!(manager.session.generation(), generation);
    assert_eq!(
        manager.session.runtime_descriptor().backend(),
        CoreRuntimeBackend::Direct
    );
}

#[tokio::test]
async fn a_capture_owner_from_another_session_is_rejected_before_authorization() {
    let original = manager();
    let other = manager();
    let mismatched = ServiceManager::new(original.session.clone(), other.capture.clone());
    let effects = AtomicUsize::new(0);
    let authorizations = AtomicUsize::new(0);
    let result = mismatched
        .authorize_then(
            mismatched.session.runtime_revision(),
            async {
                authorizations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            || async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
    assert!(matches!(result, Err(ServiceManagerError::Unsupported)));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(authorizations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn authorization_of_an_old_intent_cannot_enable_the_replacement_binding() {
    let manager = manager();
    let runtime_revision = manager.session.runtime_revision();
    let effects = AtomicUsize::new(0);
    manager
        .session
        .switch_to_direct(MihomoEndpoint {
            secret: "replacement-fixture".into(),
            ..MihomoEndpoint::default()
        })
        .await
        .unwrap();
    let generation = manager.session.generation();
    let result = manager
        .authorize_then(runtime_revision, async { Ok(()) }, || async {
            effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(ServiceManagerError::Stale)));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(manager.session.generation(), generation);
}

#[tokio::test]
async fn cancelling_authorization_never_enters_runtime_completion() {
    let manager = manager();
    let runtime_revision = manager.session.runtime_revision();
    let effects = AtomicUsize::new(0);
    let result = manager
        .authorize_then(
            runtime_revision,
            async { Err(MaintenanceError::AuthorizationCancelled) },
            || async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(ServiceManagerError::Maintenance(
            MaintenanceError::AuthorizationCancelled
        ))
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.session.generation(),
        runtime_revision.configuration_generation
    );
}

#[tokio::test]
async fn shutdown_while_authorization_waits_expires_tun_intent() {
    let manager = manager();
    let runtime_revision = manager.session.runtime_revision();
    let effects = AtomicUsize::new(0);
    let result = manager
        .authorize_then(
            runtime_revision,
            async {
                manager.session.request_shutdown();
                Ok(())
            },
            || async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
    assert!(matches!(result, Err(ServiceManagerError::Closed)));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn abandoning_the_foreground_waiter_does_not_abandon_the_command() {
    let manager = manager();
    let effects = Arc::new(AtomicUsize::new(0));
    let effects_completion = effects.clone();
    let (entered_sender, entered) = tokio::sync::oneshot::channel();
    let (continue_sender, resume) = tokio::sync::oneshot::channel();
    let waiter_manager = manager.clone();
    let waiter = tokio::spawn(async move {
        waiter_manager
            .complete(ServiceOperation::EnableTun, |_| async move {
                entered_sender.send(()).unwrap();
                resume.await.unwrap();
                effects_completion.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
    });
    entered.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let _ = continue_sender.send(());
    tokio::time::timeout(Duration::from_secs(2), async {
        while effects.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
}

#[tokio::test]
async fn a_second_command_cannot_repeat_an_authorization_in_progress() {
    let manager = manager();
    let (entered_sender, entered) = tokio::sync::oneshot::channel();
    let (continue_sender, resume) = tokio::sync::oneshot::channel();
    let first_manager = manager.clone();
    let first = tokio::spawn(async move {
        first_manager
            .complete(ServiceOperation::EnableTun, |_| async move {
                entered_sender.send(()).unwrap();
                resume.await.unwrap();
                Ok(())
            })
            .await
    });
    entered.await.unwrap();
    assert!(matches!(
        manager
            .complete::<(), _, _>(ServiceOperation::EnableTun, |_| async {
                panic!("duplicate command requested another authorization")
            })
            .await,
        Err(ServiceManagerError::Busy)
    ));
    continue_sender.send(()).unwrap();
    first.await.unwrap().unwrap();
}

#[tokio::test]
async fn independent_managers_share_the_retained_operation_slot() {
    let manager = manager();
    let independent = manager_for_session(manager.session.clone());
    let (entered_sender, entered) = tokio::sync::oneshot::channel();
    let (continue_sender, resume) = tokio::sync::oneshot::channel();
    let waiter_manager = manager.clone();
    let first = tokio::spawn(async move {
        waiter_manager
            .complete(ServiceOperation::Repair, |_| async move {
                entered_sender.send(()).unwrap();
                resume.await.unwrap();
                Ok(())
            })
            .await
    });
    entered.await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(independent.snapshot().is_busy());
    assert!(matches!(
        independent
            .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
                panic!("an independent manager repeated retained native authorization")
            })
            .await,
        Err(ServiceManagerError::Busy)
    ));
    continue_sender.send(()).unwrap();
    let mut updates = manager.session.subscribe_run_state();
    tokio::time::timeout(Duration::from_secs(2), async {
        while manager.session.run_state().op_in_flight {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
    independent
        .complete(ServiceOperation::Refresh, |_| async { Ok(()) })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_panicking_retained_command_blocks_new_managers_in_the_same_session() {
    let manager = manager();
    let independent = manager_for_session(manager.session.clone());
    let result = manager
        .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
            panic!("simulated loss of native maintenance completion")
        })
        .await;
    assert!(matches!(result, Err(ServiceManagerError::Completion(_))));
    assert_eq!(independent.snapshot().phase(), ServicePhase::Unconfirmed);
    assert!(matches!(
        independent
            .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
                panic!("unconfirmed maintenance was resubmitted")
            })
            .await,
        Err(ServiceManagerError::Busy)
    ));
}

#[tokio::test]
async fn unresolved_native_completion_cannot_be_bypassed_by_recreating_a_manager() {
    let manager = manager();
    let independent = manager_for_session(manager.session.clone());
    let error = tokio::spawn(async { panic!("native worker did not return") })
        .await
        .unwrap_err();
    let result = manager
        .complete::<(), _, _>(ServiceOperation::Uninstall, |_| async move {
            Err(ServiceManagerError::Maintenance(
                MaintenanceError::OutcomeUnconfirmed(error),
            ))
        })
        .await;
    assert!(result.unwrap_err().is_native_outcome_unconfirmed());
    assert_eq!(independent.snapshot().phase(), ServicePhase::Unconfirmed);
    assert!(matches!(
        independent.refresh_health().await,
        Err(ServiceManagerError::Busy)
    ));
}

#[tokio::test]
async fn cancelled_native_authorization_retires_pending_action_without_losing_its_error() {
    use crate::runtime::service::ipc::health::PendingAction;
    let manager = manager();
    let runtime_revision = manager.session.runtime_revision();
    let result = manager
        .authorize_action(runtime_revision, PendingAction::Install, async {
            assert_eq!(
                manager.session.run_state().pending,
                Some(PendingAction::Install)
            );
            Err(MaintenanceError::AuthorizationCancelled)
        })
        .await;
    assert!(result.unwrap_err().is_authorization_cancelled());
    assert_eq!(manager.session.run_state().pending, None);
    assert!(
        !manager
            .session
            .run_state
            .maintenance_unconfirmed
            .load(std::sync::atomic::Ordering::Acquire)
    );
}

#[tokio::test]
async fn an_unknown_native_authorization_keeps_the_requested_action_pending() {
    use crate::runtime::service::ipc::health::PendingAction;
    let manager = manager();
    let runtime_revision = manager.session.runtime_revision();
    let error = tokio::spawn(async { panic!("unknown native outcome") })
        .await
        .unwrap_err();
    let result = manager
        .complete::<(), _, _>(ServiceOperation::Repair, move |manager| async move {
            manager
                .authorize_action(
                    runtime_revision,
                    PendingAction::ForceReinstall,
                    async move { Err(MaintenanceError::OutcomeUnconfirmed(error)) },
                )
                .await
        })
        .await;
    assert!(result.unwrap_err().is_native_outcome_unconfirmed());
    assert_eq!(
        manager.session.run_state().pending,
        Some(PendingAction::ForceReinstall)
    );
    assert_eq!(manager.snapshot().phase(), ServicePhase::Unconfirmed);
}

#[tokio::test]
async fn cancelled_native_action_restores_the_same_sessions_previous_sidecar_choice() {
    use crate::runtime::service::ipc::health::PendingAction;
    let manager = manager();
    manager.session.run_state.store.accept_sidecar();
    let result = manager
        .complete(ServiceOperation::Repair, |manager| async move {
            manager
                .authorize_action(
                    manager.session.runtime_revision(),
                    PendingAction::ForceReinstall,
                    async {
                        assert!(!manager.session.run_state().sidecar_allowed);
                        Err(MaintenanceError::AuthorizationCancelled)
                    },
                )
                .await
        })
        .await;
    assert!(result.unwrap_err().is_authorization_cancelled());
    assert!(manager.session.run_state().sidecar_allowed);
    assert_eq!(manager.session.run_state().pending, None);
    assert!(!manager.session.run_state().service_needs_attention());
}

#[tokio::test]
async fn unknown_native_action_cannot_restore_a_previous_sidecar_allowance() {
    use crate::runtime::service::ipc::health::PendingAction;
    let manager = manager();
    manager.session.run_state.store.accept_sidecar();
    let result = manager
        .complete(ServiceOperation::Repair, |manager| async move {
            manager
                .authorize_action(
                    manager.session.runtime_revision(),
                    PendingAction::ForceReinstall,
                    async {
                        let error =
                            tokio::spawn(async { panic!("native fixture lost completion") })
                                .await
                                .unwrap_err();
                        Err(MaintenanceError::OutcomeUnconfirmed(error))
                    },
                )
                .await
        })
        .await;
    assert!(result.unwrap_err().is_native_outcome_unconfirmed());
    assert!(!manager.session.run_state().sidecar_allowed);
    assert_eq!(
        manager.session.run_state().pending,
        Some(PendingAction::ForceReinstall)
    );
    assert_eq!(manager.snapshot().phase(), ServicePhase::Unconfirmed);
}

#[tokio::test]
async fn competing_service_manager_returns_busy_without_marking_its_phase_failed() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let first = manager_for_session(session.clone());
    let second = manager_for_session(session);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entered_task = entered.clone();
    let release_task = release.clone();
    let first_handle = tokio::spawn({
        let first = first.clone();
        async move {
            first
                .complete(ServiceOperation::Refresh, move |_| async move {
                    entered_task.notify_one();
                    release_task.notified().await;
                    Ok(())
                })
                .await
        }
    });
    entered.notified().await;

    let busy = second
        .complete(ServiceOperation::Refresh, |_| async { Ok(()) })
        .await
        .unwrap_err();
    assert!(matches!(busy, ServiceManagerError::Busy));
    // Second handle's own local state must NOT be overwritten to Failed.
    assert_ne!(second.snapshot().phase(), ServicePhase::Failed);

    release.notify_one();
    first_handle.await.unwrap().unwrap();
}
