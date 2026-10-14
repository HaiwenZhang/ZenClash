use super::*;

#[tokio::test]
async fn service_tun_retirement_failure_keeps_saved_receipt_and_current_owner() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let binding = session.runtime_descriptor().binding_generation();
    let (generation, _) = session.accept_saved_service_tun(CommittedConfig::default(), None);
    let receipt = CoreApplyOutcome {
        kind: CoreApplyKind::Patched,
        generation,
    };
    let result = session.finish_service_tun_retirement(
        Ok(ServiceTunRuntimeOutcome {
            saved: Some(receipt),
            commit_pending: false,
            recovery_warning: None,
            failure: None,
            restored: false,
        }),
        Err(ControlledConfigError::Task(
            "retirement worker failed".into(),
        )),
    );
    assert!(result.is_ok(), "durable receipt became an ordinary error");
    let outcome = result.ok().unwrap();
    assert_eq!(outcome.saved, Some(receipt));
    assert!(!outcome.commit_pending);
    assert_eq!(
        outcome.recovery_warning,
        Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
    );
    assert_eq!(
        outcome.failure.unwrap().to_string(),
        zenclash_i18n::text("core_page.service.cleanup_unconfirmed")
    );
    assert!(!outcome.restored);
    assert_eq!(session.generation(), generation);
    assert_eq!(session.lifecycle.read().phase, CoreLifecyclePhase::Unknown);
    assert_eq!(session.runtime_descriptor().binding_generation(), binding);
}

#[tokio::test]
async fn service_tun_retirement_warning_is_independent_of_pending_commit() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let outcome = session
        .finish_service_tun_retirement(
            Ok(ServiceTunRuntimeOutcome {
                saved: Some(CoreApplyOutcome {
                    kind: CoreApplyKind::Patched,
                    generation: 1,
                }),
                commit_pending: true,
                recovery_warning: None,
                failure: Some(MihomoError::StaleTransport.into()),
                restored: false,
            }),
            Err(ControlledConfigError::Task(
                "retirement worker failed".into(),
            )),
        )
        .unwrap();
    assert!(outcome.commit_pending);
    assert_eq!(
        outcome.recovery_warning,
        Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
    );
    assert!(matches!(
        outcome.failure,
        Some(CoreSessionError::Process(MihomoError::StaleTransport))
    ));
}

#[tokio::test]
async fn service_tun_retirement_failure_preserves_original_unsaved_error() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let result = session.finish_service_tun_retirement(
        Err(MihomoError::StaleBinding.into()),
        Err(ControlledConfigError::Task(
            "retirement worker failed".into(),
        )),
    );
    assert!(matches!(
        result,
        Err(CoreSessionError::Process(MihomoError::StaleBinding))
    ));
}

#[tokio::test]
async fn pending_service_tun_target_freezes_before_deletion_and_changes_only_after_save() {
    let home = std::env::temp_dir().join(format!(
        "zenclash-tun-pending-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("rules.yaml"), "payload: [example.com]\n").unwrap();
    let payload = "mode: rule\ntun: {enable: false}\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: rules.yaml\n";
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    *session.pending_backup.write() = Some(PendingBackupRestore {
        snapshot: CoreRestoreSnapshot {
            committed: CommittedConfig::default(),
            payload: Some(payload.into()),
            service_bundle: None,
        },
        store_root: home.clone(),
        generation: 0,
    });
    let bundle = Arc::new(
        crate::ServiceRuntimeBundle::prepare(payload, home.clone())
            .await
            .unwrap(),
    );
    std::fs::remove_file(home.join("rules.yaml")).unwrap();
    let delta = serde_json::json!({"tun":{"enable":true}});
    let prepared = session
        .prepare_service_tun_backup(&home, payload, &bundle, &delta)
        .await
        .unwrap();
    assert!(
        session
            .pending_backup
            .read()
            .as_ref()
            .unwrap()
            .snapshot
            .service_bundle
            .is_none()
    );
    assert!(
        session
            .pending_backup
            .read()
            .as_ref()
            .unwrap()
            .snapshot
            .payload
            .as_ref()
            .unwrap()
            .contains("enable: false")
    );
    let (generation, conflict) =
        session.accept_saved_service_tun(CommittedConfig::default(), prepared);
    assert_eq!(generation, 1);
    assert!(!conflict);
    let pending = session.pending_backup.read().clone().unwrap();
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(pending.snapshot.payload.as_ref().unwrap()).unwrap();
    assert_eq!(yaml["tun"]["enable"], true);
    assert!(
        pending
            .snapshot
            .service_bundle
            .unwrap()
            .yaml()
            .contains("assets/")
    );
    std::fs::remove_dir(&home).unwrap();
}

#[tokio::test]
async fn later_pending_target_is_preserved_when_saved_publication_detects_conflict() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let pending = PendingBackupRestore {
        snapshot: CoreRestoreSnapshot {
            committed: CommittedConfig::default(),
            payload: Some("mode: rule\n".into()),
            service_bundle: None,
        },
        store_root: PathBuf::from("pending-root"),
        generation: 0,
    };
    *session.pending_backup.write() = Some(pending.clone());
    session.pending_backup.write().as_mut().unwrap().generation = 1;
    session
        .pending_backup
        .write()
        .as_mut()
        .unwrap()
        .snapshot
        .payload = Some("mode: global\n".into());
    let (generation, conflict) =
        session.accept_saved_service_tun(CommittedConfig::default(), Some(pending));
    assert!(conflict);
    let current = session.pending_backup.read().clone().unwrap();
    assert_eq!(current.snapshot.payload.as_deref(), Some("mode: global\n"));
    assert_eq!(current.generation, generation);
}
