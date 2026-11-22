use super::*;

fn local_manager() -> ServiceManager {
    let home = std::env::temp_dir().join("maintenance-preparation-unused-home");
    let process = crate::ManagedCoreProcess::prepare_stopped(crate::CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary: home.join("ordinary-mihomo"),
        config_file: home.join("missing-source.yaml"),
        home_dir: home,
        endpoint: crate::MihomoEndpoint::default(),
        controller_override: None,
    });
    let session = CoreSession::open(
        CoreKind::Mihomo,
        crate::MihomoClient::from_process(process).unwrap(),
    )
    .unwrap();
    let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
    ServiceManager::new(session, capture)
}

fn offline_manager(kind: CoreKind, binary: Option<PathBuf>) -> ServiceManager {
    let session = CoreSession::open_offline(
        kind,
        std::env::temp_dir().join("offline-maintenance-unused-home"),
        binary,
    )
    .unwrap();
    let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
    ServiceManager::new(session, capture)
}

#[tokio::test]
async fn offline_maintenance_prepares_without_core_profile_or_capture_changes() {
    let manager = offline_manager(
        CoreKind::Mihomo,
        Some(std::env::temp_dir().join("missing-selected-mihomo")),
    );
    let before = manager.session.runtime_descriptor();
    let capture_revision = manager.capture.capture_revision();
    for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
        let request = manager.request_maintenance(operation, None).unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        assert!(prepared.ready());
        assert!(prepared.recovery().is_none());
        assert!(prepared.request.ordinary_launch.is_none());
        assert_eq!(
            manager.session.runtime_descriptor().backend(),
            CoreRuntimeBackend::Direct
        );
        assert_eq!(
            manager.session.runtime_descriptor().binding_generation(),
            before.binding_generation()
        );
        assert_eq!(manager.session.generation(), 0);
        assert_eq!(manager.capture.capture_revision(), capture_revision);
        assert!(
            manager
                .session
                .committed_profile_snapshot()
                .profile_path
                .is_none()
        );
        assert!(matches!(
            manager.maintain_prepared(&prepared).await,
            Err(ServiceManagerError::ConsentRequired)
        ));
    }
    assert!(matches!(
        manager.request_enable_tun(),
        Err(ServiceManagerError::Unsupported)
    ));
}

#[test]
fn external_controller_cannot_acquire_offline_maintenance_from_endpoint_text() {
    let client = crate::MihomoClient::new(crate::MihomoEndpoint::new(
        "127.0.0.1:0",
        "zenclash-offline",
    ))
    .unwrap();
    let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
    assert!(!session.is_offline_recovery());
    let capture = TrafficCaptureSession::new(session.clone(), unused_store(), None, None);
    let manager = ServiceManager::new(session, capture);
    for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
        assert!(matches!(
            manager.request_maintenance(operation, None),
            Err(ServiceManagerError::Unsupported)
        ));
    }
    assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
}

#[tokio::test]
async fn rebinding_offline_session_revokes_maintenance_even_for_the_same_endpoint() {
    let manager = offline_manager(CoreKind::Mihomo, None);
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    manager
        .session
        .switch_to_direct(crate::MihomoEndpoint::new(
            "127.0.0.1:0",
            "zenclash-offline",
        ))
        .await
        .unwrap();
    assert!(!manager.session.is_offline_recovery());
    assert!(matches!(
        manager
            .maintain_prepared(&prepared.with_authorization())
            .await,
        Err(ServiceManagerError::Stale)
    ));
    assert!(matches!(
        manager.request_maintenance(ServiceOperation::Repair, None),
        Err(ServiceManagerError::Unsupported)
    ));
}

#[tokio::test]
async fn offline_preparation_rejects_another_sessions_receipt_before_native_work() {
    let manager = offline_manager(CoreKind::Mihomo, None);
    let other = offline_manager(CoreKind::Mihomo, None);
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    assert!(matches!(
        other
            .maintain_prepared(&prepared.with_authorization())
            .await,
        Err(ServiceManagerError::Stale)
    ));
    assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
}

#[tokio::test]
async fn offline_discovery_preserves_selected_source_without_parsing_a_profile() {
    let selected = std::env::temp_dir().join("offline-preferred-mihomo");
    let manager = offline_manager(CoreKind::Mihomo, Some(selected.clone()));
    let request = manager
        .discover_maintenance_request(
            ServiceOperation::Repair,
            PathBuf::from("missing-project"),
            Some(PathBuf::from("another-missing-core")),
        )
        .await
        .unwrap();
    assert!(request.ordinary_launch.is_none());
    assert_eq!(
        manager.session.offline_source().unwrap().binary.as_ref(),
        Some(&selected)
    );
    assert_eq!(manager.snapshot().revision(), 0);
}

#[tokio::test]
async fn missing_selected_offline_core_is_rejected_before_authorization_without_fallback() {
    let selected = std::env::temp_dir().join("missing-offline-selected-core");
    let manager = offline_manager(CoreKind::Mihomo, Some(selected));
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    assert!(matches!(
        manager.repair_core_source(&request).await,
        Err(ServiceManagerError::Runtime(_))
    ));
    assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    assert_eq!(manager.session.generation(), 0);
}

#[tokio::test]
async fn selected_offline_core_is_verified_without_starting_it() {
    let source = std::env::temp_dir().join(format!(
        "zenclash-offline-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&source, b"not an executable and must not be started").unwrap();
    let manager = offline_manager(CoreKind::Mihomo, Some(source.clone()));
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    assert_eq!(manager.repair_core_source(&request).await.unwrap(), source);
    assert_eq!(
        manager.session.runtime_descriptor().backend(),
        CoreRuntimeBackend::Direct
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"not an executable and must not be started"
    );
    std::fs::remove_file(source).unwrap();
}

#[test]
fn unknown_health_never_authorizes_maintenance_and_absent_uninstall_needs_no_prompt() {
    use crate::runtime::service::ipc::health::PendingAction;
    assert_eq!(
        maintenance_action(ServiceOperation::Repair, &ServiceHealth::NotInstalled).unwrap(),
        Some(PendingAction::Install)
    );
    assert_eq!(
        maintenance_action(ServiceOperation::Uninstall, &ServiceHealth::NotInstalled).unwrap(),
        None
    );
    for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
        assert!(matches!(
            maintenance_action(operation, &ServiceHealth::Unknown),
            Err(ServiceManagerError::Health(ServiceHealth::Unknown))
        ));
    }
    for health in [
        ServiceHealth::Ready,
        ServiceHealth::VersionMismatch,
        ServiceHealth::Unavailable("stopped".into()),
    ] {
        assert_eq!(
            maintenance_action(ServiceOperation::Repair, &health).unwrap(),
            Some(PendingAction::ForceReinstall)
        );
    }
}

#[test]
fn offline_maintenance_rejects_mismatched_capture_owner() {
    let manager = offline_manager(CoreKind::Mihomo, None);
    let other = offline_manager(CoreKind::Mihomo, None);
    let mismatched = ServiceManager::new(manager.session, other.capture);
    assert!(matches!(
        mismatched.request_maintenance(ServiceOperation::Repair, None),
        Err(ServiceManagerError::Unsupported)
    ));
}

#[tokio::test]
async fn offline_maintenance_rejects_shutdown_and_changed_generation() {
    let manager = offline_manager(CoreKind::Mihomo, None);
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    manager.session.invalidate_runtime_observations();
    assert!(matches!(
        manager.prepare_maintenance(request, &unused_store()).await,
        Err(ServiceManagerError::Stale)
    ));
    manager.session.shutdown().await.unwrap();
    assert!(matches!(
        manager.request_maintenance(ServiceOperation::Repair, None),
        Err(ServiceManagerError::Closed)
    ));
}

#[test]
fn offline_recovery_rejects_relative_source_paths() {
    assert!(
        CoreSession::open_offline(CoreKind::Mihomo, PathBuf::from("relative-home"), None).is_err()
    );
    assert!(
        CoreSession::open_offline(
            CoreKind::Mihomo,
            std::env::temp_dir(),
            Some(PathBuf::from("relative-core"))
        )
        .is_err()
    );
}

fn unused_store() -> crate::ControlledConfigStore {
    crate::ControlledConfigStore::new(
        std::env::temp_dir().join("maintenance-preparation-unused-store"),
    )
}

#[tokio::test]
async fn maintenance_capture_restore_preserves_preexisting_local_without_native_work() {
    let manager = local_manager();
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    let outcome = manager.restore_prepared_capture(&prepared).await.unwrap();
    assert!(outcome.tun().is_none());
    assert!(outcome.warning().is_none());
    assert_eq!(manager.session.generation(), 0);
    assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
}

#[tokio::test]
async fn maintenance_capture_restore_rejects_uninstall_and_unconfirmed_native_result() {
    let manager = local_manager();
    let request = manager
        .request_maintenance(ServiceOperation::Uninstall, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    assert!(matches!(
        manager.restore_prepared_capture(&prepared).await,
        Err(ServiceManagerError::Unsupported)
    ));
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    manager
        .state
        .send_modify(|state| state.phase = ServicePhase::Unconfirmed);
    assert!(matches!(
        manager.restore_prepared_capture(&prepared).await,
        Err(ServiceManagerError::Busy)
    ));
    assert_eq!(manager.session.generation(), 0);
}

#[tokio::test]
async fn maintenance_discovery_preserves_local_identity_without_reading_invalid_inputs() {
    let manager = local_manager();
    let before = manager.session.runtime_descriptor();
    for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
        let request = manager
            .discover_maintenance_request(
                operation,
                PathBuf::from("missing-project"),
                Some(PathBuf::from("missing-binary")),
            )
            .await
            .unwrap();
        assert_eq!(request.operation(), operation);
        assert_eq!(
            request.ordinary_launch.unwrap().binary,
            before.binary().unwrap()
        );
        assert_eq!(manager.snapshot().revision(), 0);
        assert_eq!(manager.session.generation(), 0);
    }
    assert!(matches!(
        manager
            .discover_maintenance_request(
                ServiceOperation::EnableTun,
                PathBuf::from("missing-project"),
                None,
            )
            .await,
        Err(ServiceManagerError::Unsupported)
    ));
}

#[tokio::test]
async fn service_maintenance_preparation_preserves_existing_local_without_source_reads() {
    let manager = local_manager();
    let before = manager.session.runtime_descriptor();
    for operation in [ServiceOperation::Repair, ServiceOperation::Uninstall] {
        let request = manager.request_maintenance(operation, None).unwrap();
        let prepared = manager
            .prepare_maintenance(request, &unused_store())
            .await
            .unwrap();
        assert!(prepared.ready());
        assert!(prepared.recovery().is_none());
        assert_eq!(prepared.operation(), operation);
        assert_eq!(
            manager.session.runtime_descriptor().binding_generation(),
            before.binding_generation()
        );
        assert_eq!(manager.session.generation(), 0);
        assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
    }
}

#[tokio::test]
async fn service_maintenance_preparation_rejects_equal_versions_from_another_session() {
    let first = local_manager();
    let other = local_manager();
    let request = first
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    assert!(matches!(
        other.prepare_maintenance(request, &unused_store()).await,
        Err(ServiceManagerError::Stale)
    ));
    assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
}

#[tokio::test]
async fn service_maintenance_submission_rejects_foreign_and_stale_preparation_before_native_work() {
    let manager = local_manager();
    let other = local_manager();
    let request = manager
        .request_maintenance(ServiceOperation::Repair, None)
        .unwrap();
    let prepared = manager
        .prepare_maintenance(request, &unused_store())
        .await
        .unwrap();
    assert!(matches!(
        manager.maintain_prepared(&prepared).await,
        Err(ServiceManagerError::ConsentRequired)
    ));
    assert!(matches!(
        other.maintain_prepared(&prepared).await,
        Err(ServiceManagerError::Stale)
    ));
    assert_eq!(other.snapshot().phase(), ServicePhase::Idle);
    manager.session.invalidate_runtime_observations();
    assert!(matches!(
        manager.maintain_prepared(&prepared).await,
        Err(ServiceManagerError::Stale)
    ));
    assert_eq!(manager.snapshot().revision(), 1);
}

#[tokio::test]
async fn service_maintenance_preparation_rejects_configuration_change_before_submission() {
    let manager = local_manager();
    let request = manager
        .request_maintenance(ServiceOperation::Uninstall, None)
        .unwrap();
    manager.session.invalidate_runtime_observations();
    assert!(matches!(
        manager.prepare_maintenance(request, &unused_store()).await,
        Err(ServiceManagerError::Stale)
    ));
    assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
}

#[test]
fn service_maintenance_request_rejects_nonmaintenance_operations() {
    let manager = local_manager();
    for operation in [ServiceOperation::Refresh, ServiceOperation::EnableTun] {
        assert!(matches!(
            manager.request_maintenance(operation, None),
            Err(ServiceManagerError::Unsupported)
        ));
    }
    assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
}
