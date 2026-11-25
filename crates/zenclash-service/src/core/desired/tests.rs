mod owner_tests {
    use super::super::{
        backup_legacy_state_file, clear_active_owner, commit_active_owner_session, load_active_owner,
        load_owner_desired_state, persist_active_owner, persist_owner_core_started, persist_owner_core_stopped,
        restore_desired_state, write_json_atomic,
    };
    use crate::core::auth::AuthenticatedOwner;
    use crate::{ClashConfig, CoreConfig, OwnerIdentity};
    use serial_test::serial;

    fn test_owner(uid: u32) -> AuthenticatedOwner {
        AuthenticatedOwner {
            key: uid.to_string(),
            identity: OwnerIdentity::Unix { uid, gid: 20 },
            app_data_root: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    #[serial]
    async fn unusable_restored_core_does_not_reserve_execution() -> anyhow::Result<()> {
        let owner = test_owner(90_019);
        let directory = std::env::temp_dir().join(format!("missing-core-restore-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let config = ClashConfig {
            core_config: CoreConfig {
                core_path: "/missing/mihomo".into(),
                core_ipc_path: directory.join("core.sock").to_string_lossy().into_owned(),
                ..Default::default()
            },
            log_config: Default::default(),
        };
        persist_owner_core_started(&owner, &config).await?;
        persist_active_owner(&owner).await?;
        assert!(crate::core::installation::inspect(&[], false).await?.core_busy);
        restore_desired_state().await?;
        let status = crate::core::installation::inspect(&[], false).await?;
        let occupied = status.core_busy;
        assert!(status.service_sha256.is_empty());
        clear_active_owner().await?;
        std::fs::remove_dir_all(directory)?;
        assert!(!occupied, "a failed restore with no core must permit Sidecar handoff");
        Ok(())
    }

    #[tokio::test]
    async fn desired_state_is_scoped_by_owner_key() -> anyhow::Result<()> {
        let owner_a = test_owner(90_001);
        let owner_b = test_owner(90_002);
        let config = ClashConfig {
            core_config: CoreConfig {
                core_path: "/tmp/mock-core-a".to_string(),
                ..Default::default()
            },
            log_config: Default::default(),
        };

        persist_owner_core_started(&owner_a, &config).await?;
        persist_owner_core_stopped(&owner_b).await?;

        assert!(load_owner_desired_state(&owner_a.key).await?.core_should_be_running);
        assert!(!load_owner_desired_state(&owner_b.key).await?.core_should_be_running);
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn active_owner_can_be_atomically_replaced_and_cleared() -> anyhow::Result<()> {
        let owner_a = test_owner(90_003);
        let owner_b = test_owner(90_004);

        persist_active_owner(&owner_a).await?;
        assert_eq!(
            load_active_owner()
                .await?
                .as_ref()
                .map(|state| state.owner_key.as_str()),
            Some("90003")
        );

        persist_active_owner(&owner_b).await?;
        assert_eq!(
            load_active_owner()
                .await?
                .as_ref()
                .map(|state| state.owner_key.as_str()),
            Some("90004")
        );

        clear_active_owner().await?;
        assert!(load_active_owner().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn owner_generation_survives_active_owner_clear() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_005);
        let first_token = "33".repeat(32);
        let first = commit_active_owner_session(&owner, &first_token).await?;

        assert_ne!(first.session_token_hash, first_token);
        assert!(
            !tokio::fs::read_to_string(crate::service_paths()?.active_owner_path())
                .await?
                .contains(&first_token)
        );

        clear_active_owner().await?;
        let second = commit_active_owner_session(&owner, &"44".repeat(32)).await?;

        assert!(second.generation > first.generation);
        clear_active_owner().await?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn legacy_active_owner_defaults_session_fields() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_006);
        let legacy_state = serde_json::json!({
            "owner_key": owner.key,
            "identity": owner.identity,
            "app_data_root": owner.app_data_root.to_string_lossy(),
        });
        write_json_atomic(&crate::service_paths()?.active_owner_path(), &legacy_state).await?;

        let active = load_active_owner().await?.expect("legacy owner should load");

        assert_eq!(active.generation, 0);
        assert!(active.session_token_hash.is_empty());
        clear_active_owner().await?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn session_commit_rejects_non_lowercase_hex_tokens() -> anyhow::Result<()> {
        clear_active_owner().await?;
        let owner = test_owner(90_007);

        for invalid in ["55".repeat(31), "AA".repeat(32), "gg".repeat(32)] {
            assert!(commit_active_owner_session(&owner, &invalid).await.is_err());
        }
        assert!(load_active_owner().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn corrupt_owner_desired_state_is_quarantined_and_rebuilt() -> anyhow::Result<()> {
        let owner = test_owner(90_008);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(&owner_root)?;
        let path = owner_root.join("desired-state.json");
        let damaged = b"\0damaged-json";
        std::fs::write(&path, damaged)?;

        let state = persist_owner_core_started(&owner, &ClashConfig::default()).await?;

        assert!(state.core_should_be_running);
        assert!(load_owner_desired_state(&owner.key).await?.core_should_be_running);
        let backups = std::fs::read_dir(&owner_root)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desired-state.json.corrupt-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0])?, damaged);
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn corrupt_owner_desired_state_load_returns_safe_default() -> anyhow::Result<()> {
        let owner = test_owner(90_010);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(&owner_root)?;
        let path = owner_root.join("desired-state.json");
        let damaged = b"\0damaged-before-restore";
        std::fs::write(&path, damaged)?;

        let state = load_owner_desired_state(&owner.key).await?;

        assert!(!state.core_should_be_running);
        assert!(state.last_clash_config.is_none());
        assert!(!path.exists());
        let backup = std::fs::read_dir(&owner_root)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desired-state.json.corrupt-"))
            })
            .ok_or_else(|| anyhow::anyhow!("corrupt desired state was not quarantined"))?;
        assert_eq!(std::fs::read(backup)?, damaged);
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn unavailable_owner_desired_state_does_not_reject_live_state_change() -> anyhow::Result<()> {
        let owner = test_owner(90_009);
        let owner_root = crate::service_paths()?.for_owner_key(&owner.key).root().to_path_buf();
        let _ = std::fs::remove_dir_all(&owner_root);
        std::fs::create_dir_all(owner_root.join("desired-state.json"))?;

        let state = persist_owner_core_started(&owner, &ClashConfig::default()).await?;

        assert!(state.core_should_be_running);
        assert!(owner_root.join("desired-state.json").is_dir());
        std::fs::remove_dir_all(owner_root)?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_global_state_is_backed_up_without_becoming_owner_state() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("legacy-desired-state-backup-{}", std::process::id()));
        let legacy = root.join("desired-state.json");
        let backup = root.join("desired-state.json.legacy.bak");
        std::fs::create_dir_all(&root)?;
        std::fs::write(&legacy, br#"{"core_should_be_running":true}"#)?;

        backup_legacy_state_file(&legacy).await?;

        assert!(!legacy.exists());
        assert_eq!(std::fs::read(&backup)?, br#"{"core_should_be_running":true}"#);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

mod core_path_recovery_tests {
    use super::super::core_path_is_unusable;
    use crate::ServiceErrorCode;
    use crate::core::auth::ServiceError;

    #[test]
    fn a_rejected_core_location_clears_the_desired_state_instead_of_wedging_startup() {
        let error = anyhow::Error::new(ServiceError::new(
            ServiceErrorCode::InvalidInstallLocation,
            "core path is outside /Applications",
        ))
        .context("failed to start core");

        assert!(core_path_is_unusable(&error));
    }

    #[test]
    fn a_vanished_core_binary_is_still_treated_as_unusable() {
        let error = anyhow::Error::new(ServiceError::new(
            ServiceErrorCode::InvalidRuntimeAsset,
            "core is unavailable: No such file or directory (os error 2)",
        ));

        assert!(core_path_is_unusable(&error));
    }

    #[test]
    fn a_missing_volatile_ipc_directory_no_longer_wipes_a_good_desired_state() {
        let error = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context("failed to open core IPC directory");

        assert!(!core_path_is_unusable(&error));
    }

    #[test]
    fn an_unrelated_start_failure_retains_the_desired_state() {
        let error = anyhow::anyhow!("core IPC socket did not appear");

        assert!(!core_path_is_unusable(&error));
    }
}
