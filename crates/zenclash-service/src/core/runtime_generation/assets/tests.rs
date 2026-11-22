#[cfg(unix)]
mod owner_ipc_directory_tests {
    use super::super::validate::owner_ipc_directory_is_usable;

    const DIR: platform_lib::mode_t = platform_lib::S_IFDIR | 0o700;
    const FILE: platform_lib::mode_t = platform_lib::S_IFREG | 0o600;

    #[test]
    fn accepts_a_root_owned_directory() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 0, 501, false));
    }

    #[test]
    fn accepts_a_directory_already_handed_to_the_owner() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 501, 501, false));
    }

    #[test]
    fn rejects_a_directory_belonging_to_somebody_else() {
        assert!(!owner_ipc_directory_is_usable(true, DIR, 502, 501, false));
    }

    #[test]
    fn accepts_somebody_elses_directory_only_when_the_caller_allows_it() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 502, 501, true));
    }

    #[test]
    fn rejects_anything_that_is_not_a_directory() {
        assert!(!owner_ipc_directory_is_usable(true, FILE, 0, 501, false));
        assert!(!owner_ipc_directory_is_usable(true, FILE, 501, 501, true));
    }

    #[test]
    fn rejects_a_directory_it_could_not_inspect() {
        assert!(!owner_ipc_directory_is_usable(false, DIR, 0, 501, false));
        assert!(!owner_ipc_directory_is_usable(false, DIR, 501, 501, true));
    }
}

mod approved_core_tests {
    use super::super::validate::approved_core_copy;
    use std::path::{Path, PathBuf};

    fn scratch(label: &str) -> anyhow::Result<PathBuf> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("approved-core-{label}-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn core_name() -> &'static str {
        if cfg!(windows) { "mihomo.exe" } else { "mihomo" }
    }

    #[test]
    fn a_core_named_from_a_writable_install_directory_resolves_to_the_approved_copy() -> anyhow::Result<()> {
        let cores = scratch("writable-install")?;
        let approved = cores.join(core_name());
        std::fs::write(&approved, b"approved core")?;

        let requested = Path::new(if cfg!(windows) {
            r"D:\translated software\clash\mihomo.exe"
        } else {
            "/home/someone/apps/clash/mihomo"
        });

        assert_eq!(
            approved_core_copy(&cores, requested)?,
            std::fs::canonicalize(&approved)?
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn resolving_the_approved_copy_again_yields_the_same_path() -> anyhow::Result<()> {
        let cores = scratch("idempotent")?;
        let approved = cores.join(core_name());
        std::fs::write(&approved, b"approved core")?;
        let canonical = std::fs::canonicalize(&approved)?;

        assert_eq!(approved_core_copy(&cores, &canonical)?, canonical);
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn a_core_no_installer_ever_staged_is_refused() -> anyhow::Result<()> {
        let cores = scratch("unstaged")?;

        let requested = Path::new("/somewhere").join(core_name());
        let error = approved_core_copy(&cores, &requested).expect_err("must refuse");

        assert_eq!(error.code, crate::ServiceErrorCode::InvalidInstallLocation);
        assert!(
            error.message.contains("--install-core"),
            "the error should say how to stage a core, got {:?}",
            error.message
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn installer_bookkeeping_leftovers_are_never_runnable_cores() -> anyhow::Result<()> {
        let cores = scratch("bookkeeping-leftover")?;
        for leftover in ["mihomo.exe.next", "mihomo.next", "mihomo.old"] {
            std::fs::write(cores.join(leftover), b"leftover")?;

            let error = approved_core_copy(&cores, &Path::new("/somewhere").join(leftover)).expect_err("must refuse");

            assert!(
                error.message.contains("bookkeeping suffix"),
                "unexpected message for {leftover}: {:?}",
                error.message
            );
        }
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn a_directory_standing_in_for_the_approved_core_is_refused() -> anyhow::Result<()> {
        let cores = scratch("directory")?;
        std::fs::create_dir(cores.join(core_name()))?;

        let requested = Path::new("/somewhere").join(core_name());
        let error = approved_core_copy(&cores, &requested).expect_err("must refuse");

        assert!(
            error.message.contains("not an ordinary file"),
            "unexpected message {:?}",
            error.message
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }
}

// Not under the `test` feature: that feature waives the location rule, which is the thing under test.
#[cfg(not(feature = "test"))]
mod asset_bundle_root_tests {
    use super::super::validate::ResolvedCore;

    #[test]
    fn a_writable_install_directory_provides_no_asset_bundle_root() -> anyhow::Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("bundle-root-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let core = root.join("mihomo");
        std::fs::write(&core, b"core")?;
        let requested = std::fs::canonicalize(&core)?;
        let resolved = ResolvedCore {
            executable: requested.clone(),
            requested,
        };

        assert_eq!(resolved.trusted_asset_bundle_root(), None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}

mod runtime_gc_tests {
    use super::super::materialize::{cleanup_stale_runtime_directories, snapshot_stale_runtime_directories};

    #[tokio::test]
    async fn stale_snapshot_never_collects_a_later_generation() -> anyhow::Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let owner_root = std::env::temp_dir().join(format!(
            "service-runtime-gc-snapshot-{}-{timestamp}",
            std::process::id()
        ));
        let active = owner_root.join("runtime.generation-active");
        let stale = owner_root.join("runtime.generation-stale");
        tokio::fs::create_dir_all(&active).await?;
        tokio::fs::create_dir(&stale).await?;

        let stale_paths = snapshot_stale_runtime_directories(&owner_root, &active).await;
        let later = owner_root.join("runtime.generation-later");
        tokio::fs::create_dir(&later).await?;
        cleanup_stale_runtime_directories(stale_paths, active.clone()).await;

        tokio::fs::symlink_metadata(&active).await?;
        tokio::fs::symlink_metadata(&later).await?;
        let stale_error = tokio::fs::symlink_metadata(&stale)
            .await
            .expect_err("snapshotted stale generation must be removed");
        assert_eq!(stale_error.kind(), std::io::ErrorKind::NotFound);

        let next_stale_paths = snapshot_stale_runtime_directories(&owner_root, &later).await;
        cleanup_stale_runtime_directories(next_stale_paths, later.clone()).await;
        tokio::fs::symlink_metadata(&later).await?;
        let previous_active_error = tokio::fs::symlink_metadata(&active)
            .await
            .expect_err("the next snapshot must collect the previous active generation");
        assert_eq!(previous_active_error.kind(), std::io::ErrorKind::NotFound);
        tokio::fs::remove_dir_all(owner_root).await?;
        Ok(())
    }
}

#[cfg(unix)]
mod unix_runtime_tests {
    use super::super::materialize::{PreparedRuntime, prepare_runtime};
    use crate::core::auth::{AuthenticatedOwner, ServiceError};
    use crate::core::runtime_generation::staging;
    use crate::{OwnerIdentity, RuntimeAsset, RuntimeBundle, ServiceErrorCode};
    use serial_test::serial;
    use std::path::PathBuf;

    /// Runs the two production phases together when no core is active in tests.
    async fn prepare_and_materialize(
        owner: &AuthenticatedOwner,
        bundle: &RuntimeBundle,
    ) -> Result<PreparedRuntime, ServiceError> {
        let prepared = prepare_runtime(owner, bundle).await?;
        prepared.materialize().await?;
        Ok(prepared)
    }

    fn test_owner(app_data_root: std::path::PathBuf) -> AuthenticatedOwner {
        // Unit tests do not run the installer or IPC server that normally creates this parent.
        if let Some(root) = std::path::Path::new(crate::IPC_PATH).parent() {
            std::fs::create_dir_all(root).expect("test IPC root must be creatable");
        }

        // SAFETY: `geteuid` has no preconditions or memory effects.
        let uid = unsafe { platform_lib::geteuid() };
        // SAFETY: `getegid` has no preconditions or memory effects.
        let gid = unsafe { platform_lib::getegid() };
        AuthenticatedOwner {
            key: uid.to_string(),
            identity: OwnerIdentity::Unix { uid, gid },
            app_data_root,
        }
    }

    #[tokio::test]
    #[serial]
    async fn materializes_yaml_and_assets_below_owner_runtime() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-assets-{}", std::process::id()));
        std::fs::create_dir_all(app_root.join("providers"))?;
        std::fs::write(app_root.join("providers/source.yaml"), b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner
                    .app_data_root
                    .join("providers/source.yaml")
                    .to_string_lossy()
                    .into_owned(),
                destination: "providers/copied.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let prepared = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        assert_eq!(
            std::fs::read(
                std::path::Path::new(&prepared.clash_config.core_config.config_dir).join("providers/copied.yaml")
            )?,
            b"proxies: []\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn prepared_assets_survive_legacy_source_cleanup() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-cleanup-order-{}", std::process::id()));
        let source = app_root.join("legacy-provider.yaml");
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(&source, b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let canonical_source = owner.app_data_root.join("legacy-provider.yaml");
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: canonical_source.to_string_lossy().into_owned(),
                destination: "providers/copied.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let prepared = prepare_and_materialize(&owner, &bundle).await?;
        std::fs::remove_file(source)?;

        assert_eq!(
            std::fs::read(
                std::path::Path::new(&prepared.clash_config.core_config.config_dir).join("providers/copied.yaml")
            )?,
            b"proxies: []\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn rejects_traversal_without_replacing_existing_runtime() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-traversal-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"safe")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let valid = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let prepared = prepare_and_materialize(&owner, &valid).await?;
        let invalid = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "../escape".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: valid.core_path,
        };

        let error = prepare_runtime(&owner, &invalid)
            .await
            .expect_err("traversal must fail");

        assert_eq!(error.code, ServiceErrorCode::InvalidRuntimeAsset);
        // A rejected bundle must leave the durable generation startable.
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    fn core_owned_state(prepared: &PreparedRuntime) -> PathBuf {
        PathBuf::from(&prepared.clash_config.core_config.config_dir).join("cache.db")
    }

    #[tokio::test]
    #[serial]
    async fn a_restart_keeps_the_state_the_core_wrote_for_itself() -> anyhow::Result<()> {
        // `cache.db` contains selections and must survive service-driven restarts.
        let app_root = std::env::temp_dir().join(format!("service-runtime-cachedb-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let first = prepare_and_materialize(&owner, &bundle).await?;
        std::fs::write(core_owned_state(&first), b"the node the user picked")?;

        let second = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            second.clash_config.core_config.config_dir, first.clash_config.core_config.config_dir,
            "an owner has one generation, so a restart is started against the same directory"
        );
        assert_eq!(
            std::fs::read(core_owned_state(&second))?,
            b"the node the user picked",
            "a restart must not take the core's own state away from it"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_rejected_bundle_keeps_the_state_the_core_wrote_for_itself() -> anyhow::Result<()> {
        // Rejecting a bundle must not delete core-owned state from the durable generation.
        let app_root = std::env::temp_dir().join(format!("service-runtime-cachedb-reject-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"safe")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let valid = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let prepared = prepare_and_materialize(&owner, &valid).await?;
        std::fs::write(core_owned_state(&prepared), b"the node the user picked")?;

        let invalid = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "../escape".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: valid.core_path,
        };
        prepare_runtime(&owner, &invalid)
            .await
            .expect_err("traversal must fail");

        assert_eq!(std::fs::read(core_owned_state(&prepared))?, b"the node the user picked");
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn planning_a_start_writes_nothing_into_the_generation() -> anyhow::Result<()> {
        // Planning runs while the outgoing core may still hold generation files open.
        let app_root = std::env::temp_dir().join(format!("service-runtime-planonly-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"new asset")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let core_path = app_root.join("mihomo").to_string_lossy().into_owned();
        let running = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: core_path.clone(),
        };
        let prepared = prepare_and_materialize(&owner, &running).await?;

        let candidate = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "providers/new.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path,
        };
        let planned = prepare_runtime(&owner, &candidate).await?;

        let generation = PathBuf::from(&prepared.clash_config.core_config.config_dir);
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "planning must leave the running core's configuration alone"
        );
        assert!(
            !generation.join("providers/new.yaml").exists(),
            "planning must not copy assets into a directory a core is running in"
        );

        planned.materialize().await?;

        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: global\n"
        );
        assert!(generation.join("providers/new.yaml").exists());
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn planning_a_start_does_not_replace_an_asset_the_running_core_holds() -> anyhow::Result<()> {
        // Existing destinations may be memory-mapped by the outgoing core on Windows.
        let app_root = std::env::temp_dir().join(format!("service-runtime-planonly-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("provider.yaml"), b"first\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let asset = owner.app_data_root.join("provider.yaml").to_string_lossy().into_owned();
        let running = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: asset.clone(),
                destination: "providers/one.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let live = prepare_and_materialize(&owner, &running).await?;
        let generation = PathBuf::from(&live.clash_config.core_config.config_dir);

        std::fs::write(app_root.join("provider.yaml"), b"second\n")?;
        let next = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            ..running
        };
        let planned = prepare_runtime(&owner, &next).await?;

        assert_eq!(
            std::fs::read_to_string(&live.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "planning must not replace the configuration the running core is using"
        );
        assert_eq!(
            std::fs::read(generation.join("providers/one.yaml"))?,
            b"first\n",
            "planning must not replace an asset the running core has open"
        );

        planned.materialize().await?;

        assert_eq!(
            std::fs::read_to_string(&live.clash_config.core_config.config_path)?,
            "mode: global\n"
        );
        assert_eq!(std::fs::read(generation.join("providers/one.yaml"))?, b"second\n");
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn an_unreadable_manifest_rebuilds_rather_than_refusing_to_start() -> anyhow::Result<()> {
        // Restart is staging's fallback, so corrupt bookkeeping cannot also block start.
        let app_root = std::env::temp_dir().join(format!("service-runtime-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("provider.yaml"), b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("provider.yaml").to_string_lossy().into_owned(),
                destination: "providers/one.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let first = prepare_and_materialize(&owner, &bundle).await?;
        let generation = PathBuf::from(&first.clash_config.core_config.config_dir);
        std::fs::write(generation.join(staging::MANIFEST_FILE_NAME), b"{ this is not json")?;
        std::fs::write(core_owned_state(&first), b"the node the user picked")?;
        std::fs::remove_file(generation.join("providers/one.yaml"))?;

        let second = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read(generation.join("providers/one.yaml"))?,
            b"proxies: []\n",
            "proving nothing means copying everything, not giving up"
        );
        assert_eq!(
            std::fs::read_to_string(&second.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        assert_eq!(
            std::fs::read(core_owned_state(&second))?,
            b"the node the user picked",
            "rebuilding is not a reason to take the core's own state away"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_destination_the_manifest_omits_is_copied_again() -> anyhow::Result<()> {
        // Simulate the manifest omission produced when a source changes during its copy.
        let app_root = std::env::temp_dir().join(format!("service-runtime-omitted-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("geo.dat"), b"geo bytes")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("geo.dat").to_string_lossy().into_owned(),
                destination: "geo.dat".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let first = prepare_and_materialize(&owner, &bundle).await?;
        let generation = PathBuf::from(&first.clash_config.core_config.config_dir);
        let manifest_path = generation.join(staging::MANIFEST_FILE_NAME);
        let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        assert!(
            recorded["assets"].get("geo.dat").is_some(),
            "a copy that went through cleanly is recorded"
        );

        // The copy exists but has no trusted source record.
        std::fs::write(&manifest_path, br#"{"assets":{},"remote_providers":{}}"#)?;
        std::fs::remove_file(generation.join("geo.dat"))?;

        prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read(generation.join("geo.dat"))?,
            b"geo bytes",
            "a destination the manifest omits must be copied again, not skipped"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_bundle_this_service_will_not_accept_changes_nothing() -> anyhow::Result<()> {
        // Validation must finish before any generation file changes.
        let app_root = std::env::temp_dir().join(format!("service-runtime-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("first"), b"first asset")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let core_path = app_root.join("mihomo").to_string_lossy().into_owned();
        let good = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: core_path.clone(),
        };
        let prepared = prepare_and_materialize(&owner, &good).await?;

        let asset_source = owner.app_data_root.join("first");
        let half_bad = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![
                RuntimeAsset {
                    source: asset_source.to_string_lossy().into_owned(),
                    destination: "providers/first.yaml".to_string(),
                },
                RuntimeAsset {
                    source: asset_source.to_string_lossy().into_owned(),
                    destination: "../escape".to_string(),
                },
            ],
            remote_providers: Vec::new(),
            core_path,
        };
        prepare_runtime(&owner, &half_bad)
            .await
            .expect_err("the second asset must be rejected");

        let generation = PathBuf::from(&prepared.clash_config.core_config.config_dir);
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "the configuration must still be the one a core could be started against"
        );
        assert!(
            !generation.join("providers/first.yaml").exists(),
            "the accepted asset must not have been written before the bundle was rejected"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }
}
