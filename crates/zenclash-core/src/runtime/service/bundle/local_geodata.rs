//! Fixed ordinary-home GeoData activation held through one recovery completion.

use std::future::Future;

use super::*;

/// Scoped ordinary-home authority retained by one GeoData recovery completion.
/// Escaping this value does not extend the parent completion's write authority.
pub struct LocalGeoDataRecovery {
    home: PathBuf,
    access: crate::platform::data_coordinator::DataWriteAccess,
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
        process: &Arc<crate::ManagedCoreProcess>,
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
        process: &Arc<crate::ManagedCoreProcess>,
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
        process: &Arc<crate::ManagedCoreProcess>,
    ) -> MihomoResult<crate::platform::data_coordinator::DataWriteLease> {
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
            crate::runtime::local::installation::verify_ordinary_local_executable(
                &prepared.launch_config().binary,
            )?;
            let bytes =
                crate::storage::profiles::read_profile_bytes(&prepared.launch_config().config_file)
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
        lease: crate::platform::data_coordinator::DataWriteLease,
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
            access: crate::platform::data_coordinator::DataWriteAccess::new(&home)
                .authorized(&lease),
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
            crate::platform::fs_io::atomic_write(&file.path, &file.bytes)
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
                    crate::platform::fs_io::atomic_write(&file.path, previous)
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
mod tests;
