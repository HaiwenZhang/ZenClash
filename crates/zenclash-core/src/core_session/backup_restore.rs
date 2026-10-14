//! Exact runtime replay and recovery targets owned by the original data admission.

use super::*;

impl CoreSession {
    // The caller has already reserved all session/data scopes exclusively, before swapping files.
    pub(crate) fn capture_restore_snapshot(
        &self,
        store: &ControlledConfigStore,
    ) -> Result<CoreRestoreSnapshot, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let committed = self.committed_profile.read().config.clone();
        let payload = store.cached_runtime_payload()?;
        let service_bundle = self.client.service_runtime_snapshot()?;
        Ok(CoreRestoreSnapshot {
            committed,
            payload,
            service_bundle,
        })
    }

    /// Reapplies the exact runtime payload captured before a data restore.
    ///
    /// Resolve this snapshot under the restore's exclusive write authority and
    /// pass its authorized controlled store. Source files and override documents
    /// may have changed; they are never read to recreate the previous runtime.
    ///
    /// # Errors
    ///
    /// Returns a missing-snapshot, validation, controller, process or exit error.
    pub async fn restore_snapshot(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        self.restore_snapshot_owned(store, snapshot, None, None)
            .await
    }

    /// Reserves one backup completion, using the shared persistent-data admission.
    ///
    /// # Errors
    /// Returns [`CoreSessionError::ShuttingDown`] when shutdown has started.
    pub async fn begin_backup_restore(&self) -> Result<CoreBackupAdmission, CoreSessionError> {
        self.begin_data_application().await
    }

    /// Restores an explicitly captured backup snapshot, completing admitted cleanup during shutdown.
    ///
    /// A saved service candidate is confirmed before the older immutable bundle is applied.
    /// Failed recovery retains the snapshot for [`Self::retry_backup_restore`].
    ///
    /// # Errors
    /// Returns an authority, confirmation, controller or snapshot error.
    pub async fn restore_backup_snapshot(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
        admission: &CoreBackupAdmission,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        self.restore_snapshot_owned(store, snapshot, Some(admission.clone()), None)
            .await
    }

    /// Returns the current failed backup recovery generation using only in-memory state.
    #[must_use]
    pub fn pending_backup_restore(&self) -> Option<u64> {
        self.pending_backup
            .read()
            .as_ref()
            .map(|pending| pending.generation)
    }

    /// Retries the retained exact backup snapshot without rereading provider or profile sources.
    ///
    /// # Errors
    /// Returns an exit, stale-generation, confirmation or runtime restoration error.
    pub async fn retry_backup_restore(&self) -> Result<CoreApplyOutcome, CoreSessionError> {
        let admission = self.begin_backup_restore().await?;
        let pending = self
            .pending_backup
            .read()
            .clone()
            .ok_or(CoreSessionError::NoCommittedProfile)?;
        let store = ControlledConfigStore::new(pending.store_root);
        self.restore_snapshot_owned(
            &store,
            &pending.snapshot,
            Some(admission),
            Some(pending.generation),
        )
        .await
    }

    async fn restore_snapshot_owned(
        &self,
        store: &ControlledConfigStore,
        snapshot: &CoreRestoreSnapshot,
        admission: Option<CoreBackupAdmission>,
        expected_generation: Option<u64>,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        if admission.as_ref().is_some_and(|admission| {
            !Arc::ptr_eq(&admission.inner.gate, &self.data_application_gate)
        }) {
            return Err(CoreSessionError::Process(MihomoError::StaleTransport));
        }
        let client = self.client.pin_binding()?;
        let lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mut committed = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        if admission.is_none() {
            self.ensure_running_operations_allowed()?;
            if self.pending_backup.read().is_some() {
                return Err(CoreSessionError::Process(MihomoError::StaleTransport));
            }
        }
        if expected_generation.is_some_and(|generation| generation != self.generation()) {
            return Err(CoreSessionError::Process(MihomoError::StaleTransport));
        }
        if let Some(state) = self.local_backup_recovery.read().as_ref()
            && state.binding == client.runtime_descriptor().binding_generation()
            && let Some((held, _, _)) = &state.pending
            && !snapshot
                .service_bundle
                .as_ref()
                .is_some_and(|bundle| Arc::ptr_eq(held, bundle))
        {
            return Err(MihomoError::StaleBinding.into());
        }
        let snapshot = snapshot.clone();
        let session = self.clone();
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _lease = lease;
            let _admission = admission;
            let result = session
                .restore_snapshot_admitted(
                    &store,
                    &client,
                    &snapshot,
                    &mut committed,
                    _admission
                        .as_ref()
                        .map_or(RuntimeRestoreAuthority::Ordinary, |admission| {
                            RuntimeRestoreAuthority::Backup(admission.clone())
                        }),
                )
                .await;
            if _admission.is_some() {
                let generation = session.generation();
                *session.pending_backup.write() = if result.is_err() {
                    Some(PendingBackupRestore {
                        snapshot,
                        store_root: store.root().to_path_buf(),
                        generation,
                    })
                } else {
                    None
                };
            }
            result
        })
        .await
        .map_err(|error| {
            self.invalidate_runtime_observations();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn restore_snapshot_admitted(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        snapshot: &CoreRestoreSnapshot,
        committed: &mut CommittedConfig,
        authority: RuntimeRestoreAuthority,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let mut payload = snapshot.payload.clone().ok_or_else(|| {
            CoreSessionError::Config(ControlledConfigError::Transaction(zenclash_i18n::text(
                "backup.errors.no_runtime_snapshot",
            )))
        })?;
        let mut local_recovery = None;
        let mut recovery_restart = None;
        if let (Some(crate::owned_core::OwnedCore::Local(local)), Some(bundle)) =
            (client.owned_core(), snapshot.service_bundle.as_ref())
        {
            let binding = client.runtime_descriptor().binding_generation();
            let launch = local.launch_config().config_file.clone();
            let root = store.root().join("local-runtime");
            let active = (launch == root.join("slot0/runtime.yaml")
                || launch == root.join("slot1/runtime.yaml"))
            .then_some(launch);
            if active.is_none() {
                if local.kind() != CoreKind::Mihomo
                    || local.launch_config().config_file != store.runtime_path()
                {
                    return Err(MihomoError::StaleBinding.into());
                }
                crate::verify_ordinary_local_executable(&local.launch_config().binary)?;
                recovery_restart = Some(local.clone());
            }
            let mut state = self
                .local_backup_recovery
                .read()
                .clone()
                .filter(|state| state.binding == binding)
                .unwrap_or(LocalBackupRecovery {
                    binding,
                    active,
                    pending: None,
                });
            let path = if let Some((held, path, prepared)) = &state.pending {
                if !Arc::ptr_eq(held, bundle) {
                    return Err(MihomoError::StaleBinding.into());
                }
                payload = prepared.clone();
                path.clone()
            } else {
                let (path, prepared) = bundle
                    .materialize_local_runtime_prepared(store, state.active.clone())
                    .await?;
                payload = prepared;
                state.pending = Some((bundle.clone(), path.clone(), payload.clone()));
                path
            };
            *self.local_backup_recovery.write() = Some(state);
            local.set_recovery_asset_root(Some(root));
            local_recovery = Some(path);
        }
        let process = recovery_restart;
        let kind = if process.is_some() {
            CoreApplyKind::Restarted
        } else {
            CoreApplyKind::HotReloaded
        };
        let result = store
            .restore_exact_runtime_payload(
                client,
                process,
                payload,
                snapshot.service_bundle.clone(),
                authority,
                self.shutdown_requested.clone(),
            )
            .await;
        if let Err(error) = result {
            if local_recovery.is_some()
                && !error.attempted
                && let Some(state) = self.local_backup_recovery.write().as_mut()
            {
                state.pending = None;
            }
            return Err(self.runtime_mutation_error(error));
        }
        if let Some(path) = local_recovery
            && let Some(state) = self.local_backup_recovery.write().as_mut()
        {
            state.active = Some(path);
            state.pending = None;
        }
        *committed = snapshot.committed.clone();
        Ok(CoreApplyOutcome {
            kind,
            generation: self.next_generation_with_config(Some(snapshot.committed.clone())),
        })
    }
}

pub(super) fn snapshot_with_delta(
    snapshot: &CoreRestoreSnapshot,
    delta: &serde_json::Value,
) -> Result<CoreRestoreSnapshot, CoreSessionError> {
    Ok(CoreRestoreSnapshot {
        committed: snapshot.committed.clone(),
        payload: snapshot
            .payload
            .as_deref()
            .map(|payload| crate::controlled_config::merge_held_delta(payload, delta))
            .transpose()?,
        service_bundle: snapshot
            .service_bundle
            .as_ref()
            .map(|bundle| bundle.with_delta(delta).map(Arc::new))
            .transpose()?,
    })
}
