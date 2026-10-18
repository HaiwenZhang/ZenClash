//! Real child ownership regressions; the HTTP responder is only a readiness fixture.

use std::{sync::Arc, time::Duration};

use super::test_support::OwnedCoreFixture as ChildFixture;

use crate::{
    CoreKind, CoreLaunchConfig, CoreMaintenanceIntent, CoreSession, CoreSessionError,
    ManagedCoreProcess, MihomoClient, MihomoEndpoint, MihomoError,
};

#[tokio::test]
async fn local_recovery_launch_tracks_actual_owner_and_clears_for_external_binding() {
    let directory =
        std::env::temp_dir().join(format!("zenclash-recovery-launch-{}", std::process::id()));
    let process = ManagedCoreProcess::prepare_stopped(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: directory.join("ordinary-mihomo"),
        config_file: directory.join("original.yaml"),
        home_dir: directory.join("home"),
        endpoint: MihomoEndpoint::default(),
        controller_override: None,
    });
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let count = Arc::strong_count(&process);
    let launch = session.clone().local_recovery_launch().unwrap();
    assert_eq!(launch.binary, process.launch_config().binary);
    assert_eq!(Arc::strong_count(&process), count);
    session
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();
    assert!(session.local_recovery_launch().is_none());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn maintenance_restart_failure_invalidates_an_observation_of_the_old_child() {
    let fixture = ChildFixture::new("maintenance-restart-failure").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let previous_version = session.generation();
    fixture.responder.abort();
    assert!(
        session
            .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_millis(100))
            .await
            .is_err()
    );
    assert!(fixture.process.snapshot().pid.is_none());
    assert!(
        session.generation() > previous_version,
        "a modified child retained the old read version"
    );
    assert_eq!(
        session.lifecycle_snapshot().phase,
        crate::CoreLifecyclePhase::Unknown
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_local_restart_preserves_running_lifecycle_and_pid() {
    let fixture = ChildFixture::new("rejected-restart-running").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let pid = fixture.process.snapshot().pid;
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let current = fixture.process.snapshot().pid;
    let generation = session.generation();
    session.shutdown().await.unwrap();
    assert!(matches!(
        result,
        Err(crate::CoreSessionError::Process(
            crate::MihomoError::ServiceRequired
        ))
    ));
    assert_eq!(current, pid);
    assert_eq!(generation, 0);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Stable);
    assert!(!lifecycle.stop_requested);
}

#[tokio::test]
async fn rejected_local_restart_preserves_explicit_stop_intent() {
    let fixture = ChildFixture::new("rejected-restart-stopped").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    let generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let current = session.generation();
    let running = fixture.process.snapshot().running;
    session.shutdown().await.unwrap();
    assert!(result.is_err());
    assert!(!running);
    assert_eq!(current, generation);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Stopped);
    assert!(lifecycle.stop_requested);
}

#[tokio::test]
async fn rejected_local_restart_preserves_network_resume_eligibility() {
    let fixture = ChildFixture::new("rejected-restart-network").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let capture = crate::TrafficCaptureSession::new(
        session.clone(),
        crate::ControlledConfigStore::new(fixture.directory.join("store")),
        None,
        None,
    );
    assert!(session.suspend_for_network(&capture).await.unwrap());
    let generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let result = session.maintain(CoreMaintenanceIntent::Restart).await;
    let lifecycle = session.lifecycle_snapshot();
    let rejected_generation = session.generation();
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "rules:\n  - MATCH,DIRECT\n",
    )
    .unwrap();
    let resumed = session.resume_after_network(&capture).await.unwrap();
    let running = fixture.process.snapshot().running;
    session.shutdown().await.unwrap();
    assert!(result.is_err());
    assert_eq!(rejected_generation, generation);
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::NetworkSuspended);
    assert!(!lifecycle.stop_requested);
    assert!(resumed, "rejection lost the accepted network suspension");
    assert!(running);
}

#[tokio::test]
async fn cancelled_maintenance_queue_does_not_stop_the_owned_child() {
    let fixture = ChildFixture::new("maintenance-queue-cancel").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let previous_pid = fixture.process.snapshot().pid;
    let held = session.transition.clone().lock_owned().await;
    let queued_session = session.clone();
    let queued =
        tokio::spawn(async move { queued_session.maintain(CoreMaintenanceIntent::Stop).await });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fixture.process.snapshot().pid,
        previous_pid,
        "queued cancellation still admitted a Stop"
    );
    assert_eq!(session.generation(), 0);
    assert!(!session.lifecycle_snapshot().stop_requested);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_stop_intent_is_replaced_only_by_admitted_runtime() {
    let first = ChildFixture::new("stop-intent-first").await;
    let second = ChildFixture::new("stop-intent-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    let clone = session.clone();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    assert!(clone.lifecycle_snapshot().stop_requested);
    // Missing confirmation must remain distinct from the user's stopped intention.
    session.lifecycle.write().phase = crate::CoreLifecyclePhase::Unknown;
    assert!(clone.ensure_running_operations_allowed().is_err());

    clone
        .maintain(CoreMaintenanceIntent::Restart)
        .await
        .unwrap();
    assert!(!session.lifecycle_snapshot().stop_requested);
    assert!(first.process.snapshot().pid.is_some());
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    clone
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    assert!(!session.lifecycle_snapshot().stop_requested);
    assert!(second.process.snapshot().pid.is_some());
    session.request_shutdown();
    assert!(clone.lifecycle_snapshot().stop_requested);
    assert!(clone.ensure_running_operations_allowed().is_err());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_public_permission_check_preserves_the_owned_child() {
    let fixture = ChildFixture::new("permission-queue-cancel").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let pid = fixture.process.snapshot().pid;
    let generation = session.generation();
    let held = session.transition.clone().lock_owned().await;
    let queued_session = session.clone();
    let queued = tokio::spawn(async move { queued_session.ensure_tun_permission().await });
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    drop(held);
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert_eq!(session.generation(), generation);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_permission_check_requires_existing_authority_without_changing_the_binary_or_child() {
    let fixture = ChildFixture::new("local-permission-policy").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    let pid = fixture.process.snapshot().pid;
    let binary = &fixture.process.launch_config().binary;
    let permissions = std::fs::metadata(binary).unwrap().permissions();
    let generation = session.generation();
    let result = session.ensure_tun_permission().await;
    if crate::current_process_elevated() {
        assert!(result.is_ok());
    } else {
        assert!(matches!(
            result,
            Err(CoreSessionError::Process(MihomoError::ServiceRequired))
        ));
    }
    assert_eq!(
        std::fs::metadata(binary).unwrap().permissions(),
        permissions
    );
    assert_eq!(fixture.process.snapshot().pid, pid);
    assert_eq!(session.generation(), generation);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn public_permission_check_never_restarts_an_explicitly_stopped_runtime() {
    let fixture = ChildFixture::new("permission-stopped-intent").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(fixture.process.clone()).unwrap(),
    )
    .unwrap();
    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
    let generation = session.generation();
    let _ = session.ensure_tun_permission().await;
    assert!(fixture.process.snapshot().pid.is_none());
    assert_eq!(session.generation(), generation);
    assert!(session.lifecycle_snapshot().stop_requested);
    session.request_shutdown();
    assert!(matches!(
        session.ensure_tun_permission().await,
        Err(CoreSessionError::ShuttingDown)
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_stops_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("shutdown-first").await;
    let second = ChildFixture::new("shutdown-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    session.shutdown().await.unwrap();

    assert!(first.process.snapshot().pid.is_none());
    assert!(
        second.process.snapshot().pid.is_none(),
        "shutdown leaked the current child"
    );
}

#[tokio::test]
async fn maintenance_stop_controls_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("stop-first").await;
    let second = ChildFixture::new("stop-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();

    assert!(first.process.snapshot().pid.is_none());
    assert!(
        second.process.snapshot().pid.is_none(),
        "maintenance stopped the retired child"
    );
}

#[tokio::test]
async fn maintenance_restart_never_revives_the_retired_child_after_a_binding_switch() {
    let first = ChildFixture::new("restart-first").await;
    let second = ChildFixture::new("restart-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let previous_pid = second.process.snapshot().pid;

    session
        .maintain(CoreMaintenanceIntent::Restart)
        .await
        .unwrap();

    assert!(
        first.process.snapshot().pid.is_none(),
        "maintenance revived the retired child"
    );
    let current_pid = second.process.snapshot().pid;
    assert!(current_pid.is_some());
    assert_ne!(
        current_pid, previous_pid,
        "maintenance did not restart the current child"
    );
}

#[tokio::test]
async fn session_observation_follows_the_current_child_after_a_binding_switch() {
    let first = ChildFixture::new("snapshot-first").await;
    let second = ChildFixture::new("snapshot-second").await;
    let client = MihomoClient::from_process(first.process.clone()).unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();

    let snapshot = session.snapshot();

    assert!(snapshot.managed);
    assert!(
        snapshot.running == Some(true),
        "session observed the stopped retired child"
    );
    assert_eq!(
        session.managed_process_snapshot().unwrap().pid,
        second.process.snapshot().pid
    );
}

#[tokio::test]
async fn shutdown_closes_publication_for_all_session_clones() {
    let first = ChildFixture::new("admission-first").await;
    let second = ChildFixture::new("admission-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    let clone = session.clone();
    session.request_shutdown();
    assert!(matches!(
        clone.switch_to_process(second.process.clone()).await,
        Err(crate::CoreSessionError::ShuttingDown)
    ));
    session.shutdown().await.unwrap();
    assert!(first.process.snapshot().pid.is_none());
    assert_eq!(
        session.runtime_descriptor().binary(),
        Some(first.process.launch_config().binary.as_path())
    );
}

#[tokio::test]
async fn current_owner_network_pause_and_resume_never_revive_retired_child() {
    let first = ChildFixture::new("network-first").await;
    let second = ChildFixture::new("network-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let capture = crate::TrafficCaptureSession::new(
        session.clone(),
        crate::ControlledConfigStore::new(second.directory.join("store")),
        None,
        None,
    );
    assert!(session.suspend_for_network(&capture).await.unwrap());
    assert!(second.process.snapshot().pid.is_none());
    assert!(session.resume_after_network(&capture).await.unwrap());
    assert!(second.process.snapshot().pid.is_some());
    assert!(first.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn supervisor_recovers_only_the_current_owned_child() {
    let first = ChildFixture::new("supervisor-first").await;
    let second = ChildFixture::new("supervisor-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    assert!(session.start_supervisor(&tokio::runtime::Handle::current()));
    second.process.stop_async().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while second.process.snapshot().pid.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(first.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
    assert!(second.process.snapshot().pid.is_none());
}

#[tokio::test]
async fn permission_observation_uses_current_binary_and_external_is_unsupported() {
    let first = ChildFixture::new("permission-first").await;
    let second = ChildFixture::new("permission-second").await;
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(first.process.clone()).unwrap(),
    )
    .unwrap();
    session
        .switch_to_process(second.process.clone())
        .await
        .unwrap();
    let crate::Observation::Fresh {
        value: crate::CoreTunPermissionStatus::Local(status),
        ..
    } = session.tun_permission_status().await.unwrap()
    else {
        panic!("actual local authority unavailable");
    };
    assert_eq!(
        status.binary,
        std::fs::canonicalize(&second.process.launch_config().binary).unwrap()
    );
    assert_ne!(
        status.binary,
        std::fs::canonicalize(&first.process.launch_config().binary).unwrap()
    );
    session
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();
    assert!(matches!(
        session.tun_permission_status().await.unwrap(),
        crate::Observation::Failed {
            recovery: crate::RecoveryAction::Unsupported,
            ..
        }
    ));
    assert!(matches!(
        session.ensure_tun_permission().await,
        Err(CoreSessionError::ExternalRestartUnsupported { .. })
    ));
    assert!(first.process.snapshot().pid.is_none());
    assert!(second.process.snapshot().pid.is_none());
    session.shutdown().await.unwrap();
}
