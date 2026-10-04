//! Fixed ordinary-home GeoData activation held through one recovery completion.

use std::future::Future;

use super::*;

/// Scoped ordinary-home authority retained by one GeoData recovery completion.
/// Escaping this value does not extend the parent completion's write authority.
pub struct LocalGeoDataRecovery {
    home: PathBuf,
    access: crate::data_coordinator::DataWriteAccess,
    store: crate::ControlledConfigStore,
}

impl LocalGeoDataRecovery {
    pub(crate) async fn persist_local_payload(
        &self,
        payload: String,
    ) -> crate::ControlledConfigResult<()> {
        self.store
            .persist_local_recovery_payload_admitted(payload)
            .await
    }

    /// Restarts a confirmed-stopped ordinary Mihomo using the admitted write lease.
    /// No new shared lease is queued behind a waiting restore.
    ///
    /// # Errors
    /// Rejects expired or incomplete authority, another home, a live child,
    /// non-Mihomo or privileged executables, enabled TUN, validation or readiness failures.
    /// After failure callers must confirm child termination before allowing GeoData rollback.
    pub async fn restart_local_process(
        &self,
        process: &Arc<crate::MihomoProcess>,
        timeout: std::time::Duration,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> MihomoResult<()> {
        let lease = self.admitted_lease(process).await?;
        process
            .restart_and_wait_until_with_lease(timeout, cancelled, &lease)
            .await
    }

    pub(crate) async fn validate_local_process(
        &self,
        process: &Arc<crate::MihomoProcess>,
    ) -> MihomoResult<()> {
        let lease = self.admitted_lease(process).await?;
        let validator = process.config_validator().with_write_lease(&lease);
        let path = process.launch_config().config_file.clone();
        tokio::task::spawn_blocking(move || {
            let _lease = lease;
            validator
                .validate_file(&path)
                .map_err(|error| MihomoError::Process(error.to_string()))
        })
        .await
        .map_err(|_| invalid("Local recovery validation worker failed"))?
    }

    async fn admitted_lease(
        &self,
        process: &Arc<crate::MihomoProcess>,
    ) -> MihomoResult<crate::data_coordinator::DataWriteLease> {
        let access = self.access.clone();
        let home = self.home.clone();
        let store_root = self.store.root().to_path_buf();
        let prepared = process.clone();
        tokio::task::spawn_blocking(move || {
            if prepared.kind() != crate::CoreKind::Mihomo
                || prepared.launch_config().home_dir != home
                || prepared.snapshot().running
            {
                return Err(invalid(
                    "Local GeoData recovery requires its stopped Mihomo owner",
                ));
            }
            let lease = access
                .borrowed_authority(&prepared.write_scopes())
                .map_err(|_| invalid("Local recovery authority does not cover its runtime"))?
                .ok_or_else(|| invalid("Local GeoData recovery authority has expired"))?;
            crate::verify_ordinary_local_executable(&prepared.launch_config().binary)?;
            let bytes = crate::profiles::read_profile_bytes(&prepared.launch_config().config_file)
                .map_err(|_| invalid("Cannot read local recovery configuration"))?;
            let value: Value = serde_yaml::from_slice(&bytes)
                .map_err(|_| invalid("Invalid local recovery configuration"))?;
            if let Some(tun) = value.get("tun") {
                let tun = tun
                    .as_mapping()
                    .ok_or_else(|| invalid("Invalid local recovery TUN definition"))?;
                if let Some(enable) = tun.get(Value::from("enable"))
                    && enable.as_bool() != Some(false)
                {
                    return Err(invalid("Local recovery configuration must disable TUN"));
                }
            }
            let slot = prepared.launch_config().config_file.parent();
            let root = store_root.join("local-runtime");
            let asset_root = slot
                .filter(|slot| *slot == root.join("slot0") || *slot == root.join("slot1"))
                .filter(|_| {
                    prepared.launch_config().config_file.file_name()
                        == Some("runtime.yaml".as_ref())
                })
                .map(|_| root);
            prepared.set_recovery_asset_root(asset_root);
            Ok::<_, MihomoError>(lease)
        })
        .await
        .map_err(|_| invalid("Local recovery preparation worker failed"))?
    }
}

struct GeoDataFile {
    path: PathBuf,
    bytes: Arc<[u8]>,
    previous: Option<Vec<u8>>,
}

struct GeoDataActivation {
    files: Vec<GeoDataFile>,
    changed: usize,
}

impl ServiceRuntimeBundle {
    /// Activates held named GeoData for one ordinary-home recovery completion.
    ///
    /// Call only after all kernels using this home have been confirmed stopped.
    /// A completion that starts a kernel must confirm its stop before returning an error;
    /// an unconfirmed live kernel cannot authorize filesystem rollback.
    /// Completion failure restores prior files, including previously absent paths.
    /// Admission retains its lease and store gate if the awaiting caller is cancelled.
    /// The completion must not reacquire this store's mutation gate or core transition.
    /// This does not stop a service, start a kernel, or publish a runtime binding.
    ///
    /// # Errors
    /// Rejects invalid homes, links, unbounded backups, activation or completion failures.
    /// A rollback failure is reported separately and must block subsequent startup.
    pub async fn with_local_geodata<T, C, F>(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        home: PathBuf,
        completion: C,
    ) -> MihomoResult<T>
    where
        T: Send + 'static,
        C: FnOnce(LocalGeoDataRecovery) -> F + Send + 'static,
        F: Future<Output = MihomoResult<T>> + Send + 'static,
    {
        let lease = store
            .acquire_write_lease_for_paths(vec![home.clone()])
            .await
            .map_err(|error| invalid(&error.to_string()))?;
        let mutation = store.lock_service_tun_mutation().await;
        self.with_local_geodata_admitted(store, home, lease, mutation, completion)
            .await
    }

    pub(crate) async fn with_local_geodata_admitted<T, C, F>(
        self: &Arc<Self>,
        store: &crate::ControlledConfigStore,
        home: PathBuf,
        lease: crate::data_coordinator::DataWriteLease,
        mutation: tokio::sync::OwnedMutexGuard<()>,
        completion: C,
    ) -> MihomoResult<T>
    where
        T: Send + 'static,
        C: FnOnce(LocalGeoDataRecovery) -> F + Send + 'static,
        F: Future<Output = MihomoResult<T>> + Send + 'static,
    {
        if !store.owns_service_tun_mutation(&mutation) {
            return Err(invalid("GeoData recovery has another store mutation gate"));
        }
        if !lease.covers(store.root()) || !lease.covers(&home) {
            return Err(invalid(
                "GeoData recovery lease does not cover its store and home",
            ));
        }
        let recovery = LocalGeoDataRecovery {
            access: crate::data_coordinator::DataWriteAccess::new(&home).authorized(&lease),
            home: home.clone(),
            store: store.with_write_lease(&lease),
        };
        let bundle = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let _mutation = mutation;
            let activation = tokio::task::spawn_blocking(move || {
                let mut activation = prepare(&bundle, &home)?;
                if let Err(error) = activation.apply() {
                    activation.rollback()?;
                    return Err(error);
                }
                Ok::<_, MihomoError>(activation)
            })
            .await
            .map_err(|_| invalid("GeoData activation worker failed"))??;
            match completion(recovery).await {
                Ok(value) => Ok(value),
                Err(error) => {
                    tokio::task::spawn_blocking(move || activation.rollback())
                        .await
                        .map_err(|_| invalid("GeoData rollback worker failed"))??;
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| invalid("GeoData recovery completion failed"))?
    }
}

fn prepare(bundle: &ServiceRuntimeBundle, home: &Path) -> MihomoResult<GeoDataActivation> {
    if !home.is_absolute()
        || home.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(invalid("GeoData home must be absolute"));
    }
    local_runtime::check_ancestors(home)?;
    if !home.is_dir() {
        return Err(invalid(
            "GeoData recovery requires the existing ordinary home",
        ));
    }
    let mut files = Vec::new();
    let mut backup_bytes = 0usize;
    let mut new_bytes = 0usize;
    for asset in &bundle.assets {
        let Some(name) = asset.path.strip_prefix("assets/geodata/") else {
            continue;
        };
        if !GEODATA.contains(&name) && name != "country.mmdb" {
            return Err(invalid("Unknown named GeoData resource"));
        }
        let path = if name == "country.mmdb" {
            mmdb_source(home)?.unwrap_or_else(|| home.join("country.mmdb"))
        } else {
            home.join(name)
        };
        if files.iter().any(|file: &GeoDataFile| file.path == path) {
            return Err(invalid("Duplicate named GeoData resource"));
        }
        let previous = read_previous(&path, MAX_TOTAL_BYTES - backup_bytes)?;
        backup_bytes = backup_bytes
            .checked_add(previous.as_ref().map_or(0, Vec::len))
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(|| invalid("GeoData rollback backup exceeds its budget"))?;
        new_bytes = new_bytes
            .checked_add(asset.bytes.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(|| invalid("GeoData activation exceeds its budget"))?;
        if asset.bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(invalid("GeoData activation file exceeds its budget"));
        }
        files.push(GeoDataFile {
            path,
            bytes: asset.bytes.clone(),
            previous,
        });
    }
    Ok(GeoDataActivation { files, changed: 0 })
}

fn read_previous(path: &Path, remaining: usize) -> MihomoResult<Option<Vec<u8>>> {
    local_runtime::check_ancestors(
        path.parent()
            .ok_or_else(|| invalid("GeoData home missing"))?,
    )?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !local_runtime::linked(&metadata) && metadata.is_file() => {
            read_asset_with_limit(path, remaining as u64)
                .map(Some)
                .map_err(|_| invalid("Cannot back up named GeoData"))
        }
        Ok(_) => Err(invalid(
            "Named GeoData destination is linked or not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(invalid("Cannot inspect named GeoData destination")),
    }
}

impl GeoDataActivation {
    fn apply(&mut self) -> MihomoResult<()> {
        for file in &self.files {
            // Repeat destination classification before writes; no imported source path is used.
            check_destination(&file.path)?;
            crate::profiles::atomic_write(&file.path, &file.bytes)
                .map_err(|_| invalid("Named GeoData activation failed"))?;
            self.changed += 1;
        }
        Ok(())
    }

    fn rollback(self) -> MihomoResult<()> {
        let mut failed = false;
        for file in self.files[..self.changed].iter().rev() {
            let result = check_destination(&file.path).and_then(|()| {
                if let Some(previous) = &file.previous {
                    crate::profiles::atomic_write(&file.path, previous)
                        .map_err(|_| invalid("Named GeoData restore failed"))
                } else {
                    match std::fs::remove_file(&file.path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(_) => Err(invalid("Named GeoData restore failed")),
                    }
                }
            });
            failed |= result.is_err();
        }
        if failed {
            Err(MihomoError::Process(
                "GeoData rollback failed; local startup must remain blocked".into(),
            ))
        } else {
            Ok(())
        }
    }
}

fn check_destination(path: &Path) -> MihomoResult<()> {
    local_runtime::check_ancestors(
        path.parent()
            .ok_or_else(|| invalid("GeoData home missing"))?,
    )?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !local_runtime::linked(&metadata) => Ok(()),
        Ok(_) => Err(invalid(
            "Named GeoData destination is linked or not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(invalid("Cannot inspect named GeoData destination")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn geodata_previous_file_respects_remaining_backup_budget() {
        let home = super::super::tests::TestHome::new();
        let path = home.0.join("GeoIP.dat");
        fs::write(&path, [1, 2]).unwrap();
        assert!(read_previous(&path, 1).is_err());
        assert_eq!(read_previous(&path, 2).unwrap(), Some(vec![1, 2]));
        fs::write(&path, []).unwrap();
        assert_eq!(read_previous(&path, 0).unwrap(), Some(vec![]));
    }

    fn bundle(files: &[(&str, &[u8])]) -> Arc<ServiceRuntimeBundle> {
        Arc::new(ServiceRuntimeBundle {
            yaml: "mode: rule\n".into(),
            assets: files
                .iter()
                .map(|(name, bytes)| RuntimeAsset {
                    path: format!("assets/geodata/{name}"),
                    bytes: Arc::from(*bytes),
                })
                .collect(),
        })
    }

    #[tokio::test]
    async fn geodata_recovery_rejects_uncovered_home_without_panicking_or_writing() {
        let home = super::super::tests::TestHome::new();
        let ordinary_home = home.0.join("ordinary");
        fs::create_dir(&ordinary_home).unwrap();
        fs::write(ordinary_home.join("GeoIP.dat"), b"original").unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let lease = store.acquire_write_lease().await.unwrap();
        let mutation = store.lock_service_tun_mutation().await;
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        let result = bundle(&[("GeoIP.dat", b"replacement")])
            .with_local_geodata_admitted(
                &store,
                ordinary_home.clone(),
                lease,
                mutation,
                move |_| async move {
                    observed.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                },
            )
            .await;
        assert!(result.is_err());
        assert!(!called.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            fs::read(ordinary_home.join("GeoIP.dat")).unwrap(),
            b"original"
        );
    }

    #[tokio::test]
    async fn geodata_recovery_published_stopped_owner_starts_and_session_shutdown_reaps_it() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-new-owner").await;
        fixture.process.stop_async().await.unwrap();
        let process =
            crate::MihomoProcess::prepare_stopped(fixture.process.launch_config().clone());
        assert!(process.snapshot().pid.is_none());
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let mutation = client
            .publish_prepared_process(process.clone(), mutation)
            .await
            .unwrap();
        let home = process.launch_config().home_dir.clone();
        let store = crate::ControlledConfigStore::new(home.parent().unwrap());
        let next = process.clone();
        bundle(&[])
            .with_local_geodata(&store, home, move |recovery| async move {
                let _mutation = mutation;
                recovery
                    .restart_local_process(&next, std::time::Duration::from_secs(2), None)
                    .await
            })
            .await
            .unwrap();
        assert!(process.snapshot().pid.is_some());
        assert!(fixture.process.snapshot().pid.is_none());
        assert_eq!(
            client.runtime_descriptor().binary(),
            Some(process.launch_config().binary.as_path())
        );
        session.shutdown().await.unwrap();
        assert!(process.snapshot().pid.is_none());
    }

    #[tokio::test]
    async fn geodata_recovery_published_stopped_owner_failure_keeps_owner_and_rolls_back_files() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-new-owner-failure")
                .await;
        fixture.process.stop_async().await.unwrap();
        let mut launch = fixture.process.launch_config().clone();
        launch.binary = launch.home_dir.join("missing-mihomo");
        let process = crate::MihomoProcess::prepare_stopped(launch);
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let mutation = client
            .publish_prepared_process(process.clone(), mutation)
            .await
            .unwrap();
        let home = process.launch_config().home_dir.clone();
        fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
        let store = crate::ControlledConfigStore::new(home.parent().unwrap());
        let next = process.clone();
        assert!(
            bundle(&[("GeoIP.dat", b"new geoip")])
                .with_local_geodata(&store, home.clone(), move |recovery| async move {
                    let _mutation = mutation;
                    recovery
                        .restart_local_process(&next, std::time::Duration::from_millis(200), None)
                        .await
                })
                .await
                .is_err()
        );
        assert!(process.snapshot().pid.is_none());
        assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
        assert_eq!(
            client.runtime_descriptor().binary(),
            Some(process.launch_config().binary.as_path())
        );
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn geodata_recovery_expired_authority_cannot_start_a_child_or_acquire_a_new_lease() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-expired-authority")
                .await;
        fixture.process.stop_async().await.unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        let store = crate::ControlledConfigStore::new(home.parent().unwrap());
        let recovery = bundle(&[])
            .with_local_geodata(&store, home, |recovery| async { Ok(recovery) })
            .await
            .unwrap();
        assert!(
            recovery
                .restart_local_process(
                    &fixture.process,
                    std::time::Duration::from_millis(200),
                    None
                )
                .await
                .is_err()
        );
        assert!(fixture.process.snapshot().pid.is_none());
        fs::write(store.runtime_path(), "tun:\n  enable: true\n").unwrap();
        let previous_cache = fs::read(store.runtime_path()).unwrap();
        let previous_patch = fs::read(store.root().join("override.yaml")).ok();
        assert!(
            recovery
                .persist_local_payload("tun:\n  enable: false\n".to_owned())
                .await
                .is_err()
        );
        assert_eq!(fs::read(store.runtime_path()).unwrap(), previous_cache);
        assert_eq!(
            fs::read(store.root().join("override.yaml")).ok(),
            previous_patch
        );
    }

    #[tokio::test]
    async fn geodata_recovery_enabled_tun_rejects_start_and_restores_named_files() {
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-tun-rejection").await;
        fixture.process.stop_async().await.unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
        fs::write(
            &fixture.process.launch_config().config_file,
            "tun:\n  enable: true\n",
        )
        .unwrap();
        let store = crate::ControlledConfigStore::new(home.parent().unwrap());
        let process = fixture.process.clone();
        assert!(
            bundle(&[("GeoIP.dat", b"new geoip")])
                .with_local_geodata(&store, home.clone(), move |recovery| async move {
                    recovery
                        .restart_local_process(
                            &process,
                            std::time::Duration::from_millis(200),
                            None,
                        )
                        .await
                })
                .await
                .is_err()
        );
        assert!(fixture.process.snapshot().pid.is_none());
        assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
    }

    #[tokio::test]
    async fn geodata_recovery_restart_does_not_deadlock_behind_waiting_restore() {
        use std::{
            sync::atomic::{AtomicBool, Ordering},
            time::Duration,
        };
        let fixture =
            crate::core_session::ownership_tests::ChildFixture::new("geodata-restart-authority")
                .await;
        fixture.process.stop_async().await.unwrap();
        let home = fixture.process.launch_config().home_dir.clone();
        fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
        let store = crate::ControlledConfigStore::new(home.parent().unwrap());
        let process = fixture.process.clone();
        let restarted = Arc::new(AtomicBool::new(false));
        let observed = restarted.clone();
        let restore_home = home.clone();
        let (restore_sender, restore_receiver) = tokio::sync::oneshot::channel();
        let result = bundle(&[("GeoIP.dat", b"new geoip")])
            .with_local_geodata(&store, home.clone(), move |recovery| async move {
                let restore_path = restore_home.clone();
                let restore = tokio::task::spawn_blocking(move || {
                    crate::data_coordinator::DataWriteLease::exclusive([restore_path])
                });
                restore_sender.send(restore).unwrap();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !crate::data_coordinator::has_waiting_restore(&restore_home) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let start = tokio::time::timeout(
                    Duration::from_millis(500),
                    recovery.restart_local_process(&process, Duration::from_millis(200), None),
                )
                .await;
                observed.store(matches!(start, Ok(Ok(()))), Ordering::SeqCst);
                tokio::time::timeout(
                    Duration::from_millis(500),
                    recovery.persist_local_payload("tun:\n  enable: false\n".to_owned()),
                )
                .await
                .expect("local persistence queued behind its own waiting restore")
                .unwrap();
                process.stop_async().await.unwrap();
                Err::<(), _>(invalid("Local recovery rejected after confirmed stop"))
            })
            .await;
        assert!(result.is_err());
        let restore = restore_receiver.await.unwrap();
        let lease = tokio::time::timeout(Duration::from_secs(3), restore)
            .await
            .unwrap()
            .unwrap();
        drop(lease);
        assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
        assert!(
            restarted.load(Ordering::SeqCst),
            "local restart queued behind a restore waiting for its own outer lease"
        );
    }

    #[tokio::test]
    async fn geodata_recovery_success_preserves_mmdb_selection_and_unrelated_files() {
        let home = super::super::tests::TestHome::new();
        fs::write(home.0.join("geoip.metadb"), b"old selected").unwrap();
        fs::write(home.0.join("unrelated"), b"untouched").unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let selected = home.0.join("geoip.metadb");
        bundle(&[("country.mmdb", b"new selected"), ("ASN.mmdb", b"new asn")])
            .with_local_geodata(&store, home.0.clone(), move |_| async move {
                assert_eq!(fs::read(selected).unwrap(), b"new selected");
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            fs::read(home.0.join("geoip.metadb")).unwrap(),
            b"new selected"
        );
        assert!(!home.0.join("country.mmdb").exists());
        assert_eq!(fs::read(home.0.join("ASN.mmdb")).unwrap(), b"new asn");
        assert_eq!(fs::read(home.0.join("unrelated")).unwrap(), b"untouched");
    }

    #[tokio::test]
    async fn geodata_recovery_failure_restores_prior_bytes_and_prior_absence() {
        let home = super::super::tests::TestHome::new();
        fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let result = bundle(&[("GeoIP.dat", b"new geoip"), ("geosite.dat", b"new sites")])
            .with_local_geodata(&store, home.0.clone(), |_| async {
                Err::<(), _>(invalid("Local start rejected"))
            })
            .await;
        assert!(result.is_err());
        assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
        assert!(!home.0.join("geosite.dat").exists());
    }

    #[tokio::test]
    async fn geodata_recovery_preflight_rejects_bad_later_destination_before_any_write() {
        let home = super::super::tests::TestHome::new();
        fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
        fs::create_dir(home.0.join("ASN.mmdb")).unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        assert!(
            bundle(&[("GeoIP.dat", b"new geoip"), ("ASN.mmdb", b"new asn")])
                .with_local_geodata::<(), _, _>(&store, home.0.clone(), |_| async {
                    panic!("invalid preflight must not start completion")
                })
                .await
                .is_err()
        );
        assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
    }

    #[tokio::test]
    async fn geodata_recovery_cancelled_waiter_retains_gate_until_completion_and_rollback() {
        let home = super::super::tests::TestHome::new();
        fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
        let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
        let owned_store = store.clone();
        let path = home.0.clone();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            bundle(&[("GeoIP.dat", b"new geoip")])
                .with_local_geodata(&owned_store, path, move |_| async move {
                    entered.send(()).unwrap();
                    resumed.await.unwrap();
                    Err::<(), _>(invalid("Local start rejected"))
                })
                .await
        });
        entering.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"new geoip");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                store.lock_service_tun_mutation()
            )
            .await
            .is_err()
        );
        resume.send(()).unwrap();
        let _gate = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            store.lock_service_tun_mutation(),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
    }
}
