//! Effective configuration and profile admission through the original session transition.

use super::*;

pub(crate) struct PreparedLocalConfig {
    pub(super) update: crate::ControlledConfigUpdate,
    pub(super) expected_cache: Option<String>,
    pub(super) expected_runtime: RuntimeConfigurationRevision,
    pub(super) session_generation: Arc<AtomicU64>,
    pub(super) store_root: PathBuf,
    pub(super) committed: CommittedConfig,
    pub(super) persist_patch: bool,
}

impl CoreProfileApplication {
    pub(crate) async fn commit(
        mut self,
        profile: PathBuf,
    ) -> Result<Option<CoreApplyOutcome>, (CoreSessionError, u64)> {
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, kind } => {
                let config = CommittedConfig {
                    profile: Some(profile),
                    overrides: self.overrides,
                };
                *self.transition_guard = config.clone();
                self.client.invalidate_connections();
                let generation = advance_profile_snapshot(
                    &self.generation,
                    &self.committed_profile,
                    &self.pending_backup,
                    Some(config),
                );
                // The catalog is already durable. Publish its identity even if finalization loses its ack.
                transaction
                    .commit()
                    .await
                    .map_err(|error| (CoreSessionError::Config(error), generation))?;
                Ok(Some(CoreApplyOutcome { kind, generation }))
            }
            CoreProfileApplicationState::Validated(_validation) => Ok(None),
        }
    }

    pub(crate) async fn rollback(self) -> CoreProfileRollbackOutcome {
        self.client.invalidate_connections();
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, .. } => {
                let result = transaction
                    .rollback()
                    .await
                    .map_err(CoreSessionError::Config);
                // Readers may have observed the accepted candidate before persistence failed.
                let runtime_version = advance_profile_snapshot(
                    &self.generation,
                    &self.committed_profile,
                    &self.pending_backup,
                    None,
                );
                CoreProfileRollbackOutcome::Runtime {
                    result,
                    runtime_version,
                }
            }
            CoreProfileApplicationState::Validated(_) => CoreProfileRollbackOutcome::Validated,
        }
    }
}

impl CoreSession {
    /// Applies an effective-configuration intent through hot reload or managed restart.
    ///
    /// Explicit API rejection is returned directly. A managed core falls back
    /// to restart only after transport or response-decoding failure, where the
    /// hot-reload outcome is uncertain. External cores are never restarted.
    ///
    /// # Errors
    ///
    /// Returns preparation, validation, controller, persistence, or process errors.
    pub async fn apply(
        &self,
        store: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let _write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&_write_lease);
        let client = client.with_write_lease(&_write_lease)?;
        let active_profile = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let session = self.clone();
        // At most one admitted completion task owns this transition; shutdown waits for it.
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _write_lease = _write_lease;
            session
                .apply_admitted(store, client, active_profile, intent)
                .await
        })
        .await
        .map_err(|error| {
            self.invalidate_runtime_observations();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn apply_admitted(
        &self,
        store: ControlledConfigStore,
        client: MihomoClient,
        mut active_profile: tokio::sync::OwnedMutexGuard<CommittedConfig>,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        if matches!(&intent, EffectiveConfigIntent::ReapplyCurrent { .. })
            && active_profile.profile.is_none()
        {
            return Err(CoreSessionError::NoCommittedProfile);
        }
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let (kind, profile, active_overrides, receipt) = match intent {
            EffectiveConfigIntent::Patch {
                profile,
                patch,
                overrides,
            } => {
                let (profile, overrides) = active_profile
                    .profile
                    .as_ref()
                    .map_or((profile, overrides), |profile| {
                        (profile.clone(), active_profile.overrides.clone())
                    });
                if client.service_client().is_some()
                    && crate::controlled_config::service_partial_patch(&patch)
                {
                    self.validate_backup_delta(&patch)?;
                }
                let (kind, receipt) = self
                    .apply_patch(&store, &client, profile.clone(), patch, overrides.clone())
                    .await?;
                (kind, profile, overrides, Some(receipt))
            }
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                let kind = self
                    .activate_profile(&store, &client, profile.clone(), overrides.clone())
                    .await?;
                (kind, profile, overrides, None)
            }
            EffectiveConfigIntent::ReapplyCurrent { overrides } => {
                let profile = active_profile
                    .profile
                    .clone()
                    .ok_or(CoreSessionError::NoCommittedProfile)?;
                let kind = self
                    .activate_profile(&store, &client, profile.clone(), overrides.clone())
                    .await?;
                (kind, profile, overrides, None)
            }
        };
        let committed = CommittedConfig {
            profile: Some(profile),
            overrides: active_overrides,
        };
        *active_profile = committed.clone();
        let generation = if let Some(receipt) = receipt {
            let generation = self.accept_saved_delta(Some(committed), receipt.delta.as_ref())?;
            receipt.confirmation?;
            generation
        } else {
            self.next_generation_with_config(Some(committed))
        };
        Ok(CoreApplyOutcome { kind, generation })
    }

    /// Changes the outbound mode on the currently committed profile.
    ///
    /// Resolves profile identity after entering the same transition gate used
    /// for profile application, maintenance and shutdown. An attached session
    /// without a source profile updates only the live controller.
    ///
    /// # Errors
    ///
    /// Returns controller, persistence or lifecycle errors. Failures after a
    /// runtime mutation attempt invalidate reads by advancing the generation;
    /// preflight rejection leaves the current generation unchanged.
    pub async fn set_mode(
        &self,
        store: &ControlledConfigStore,
        mode: &str,
    ) -> Result<u64, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let _write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&_write_lease);
        let client = client.with_write_lease(&_write_lease)?;
        let active_profile = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let session = self.clone();
        let mode = mode.to_owned();
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _write_lease = _write_lease;
            session
                .set_mode_admitted(store, client, active_profile, &mode)
                .await
        })
        .await
        .map_err(|error| {
            self.invalidate_runtime_observations();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    async fn set_mode_admitted(
        &self,
        store: ControlledConfigStore,
        client: MihomoClient,
        active_profile: tokio::sync::OwnedMutexGuard<CommittedConfig>,
        mode: &str,
    ) -> Result<u64, CoreSessionError> {
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        let requested = serde_json::json!({"mode":mode.trim().to_ascii_lowercase()});
        self.validate_backup_delta(&requested)?;
        if let Some(profile) = active_profile.profile.as_ref() {
            let receipt = store
                .apply_mode_update_for_session(
                    &client,
                    profile,
                    mode,
                    active_profile.overrides.clone(),
                )
                .await
                .map_err(|error| self.runtime_mutation_error(error))?;
            let generation = self.accept_saved_delta(None, receipt.delta.as_ref())?;
            receipt.confirmation?;
            return Ok(generation);
        } else if let Err(error) = client.set_mode(mode).await {
            if !matches!(
                &error,
                MihomoError::InvalidInput(_) | MihomoError::StaleBinding
            ) {
                self.next_generation();
            }
            return Err(error.into());
        }
        self.accept_saved_delta(None, Some(&requested))
    }

    #[cfg(test)]
    pub(crate) async fn stage_profile_application(
        &self,
        store: &ControlledConfigStore,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
        apply_runtime: bool,
    ) -> Result<CoreProfileApplication, CoreProfileStageError> {
        self.stage_profile_application_expected(
            store,
            candidate,
            _previous,
            overrides,
            apply_runtime,
            None,
        )
        .await
    }

    pub(crate) async fn stage_profile_application_expected(
        &self,
        store: &ControlledConfigStore,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
        apply_runtime: bool,
        expected: Option<RuntimeConfigurationRevision>,
    ) -> Result<CoreProfileApplication, CoreProfileStageError> {
        let candidate = candidate.into();
        let client = self.client.pin_binding().map_err(CoreSessionError::from)?;
        let write_lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client
            .ensure_binding_current()
            .map_err(CoreSessionError::from)?;
        let store = store.with_write_lease(&write_lease);
        let client = client
            .with_write_lease(&write_lease)
            .map_err(CoreSessionError::from)?;
        let transition_guard = self.transition.clone().lock_owned().await;
        client
            .ensure_binding_current()
            .map_err(CoreSessionError::from)?;
        self.ensure_running_operations_allowed()?;
        if let Some(expected) = expected
            && (client.runtime_descriptor().binding_generation() != expected.binding_generation
                || self.generation() != expected.configuration_generation)
        {
            return Err(CoreSessionError::from(MihomoError::StaleBinding).into());
        }
        let availability_pause = apply_runtime.then(|| self.pause_pac_availability());
        let active_overrides = overrides.clone();
        let state = if !apply_runtime {
            CoreProfileApplicationState::Validated(
                store
                    .stage_profile_validation(self.kind, &client, candidate, overrides)
                    .await?,
            )
        } else {
            let (transaction, kind) = store
                .stage_profile_reload_with_restart(
                    &client,
                    candidate,
                    overrides,
                    self.shutdown_requested.clone(),
                )
                .await
                .map_err(|error| self.profile_stage_error(error))?;
            CoreProfileApplicationState::Runtime {
                transaction: Box::new(transaction),
                kind,
            }
        };
        Ok(CoreProfileApplication {
            state,
            generation: self.generation.clone(),
            committed_profile: self.committed_profile.clone(),
            pending_backup: self.pending_backup.clone(),
            client,
            transition_guard,
            overrides: active_overrides,
            _write_lease: write_lease,
            _availability_pause: availability_pause,
        })
    }

    async fn apply_patch(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<(CoreApplyKind, SavedRuntimeReceipt), CoreSessionError> {
        store
            .apply_json_update_with_fallback_for_session(
                client,
                profile,
                &patch,
                overrides,
                self.shutdown_requested.clone(),
            )
            .await
            .map_err(|error| self.runtime_mutation_error(error))
    }

    async fn activate_profile(
        &self,
        store: &ControlledConfigStore,
        client: &MihomoClient,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        store
            .reload_with_fallback_for_session(
                client,
                profile,
                overrides,
                self.shutdown_requested.clone(),
            )
            .await
            .map_err(|error| self.runtime_mutation_error(error))
    }

    pub(super) fn local_process(
        &self,
        client: &MihomoClient,
    ) -> Result<Arc<ManagedCoreProcess>, CoreSessionError> {
        client.ensure_binding_current()?;
        match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => Ok(process),
            _ => Err(CoreSessionError::ExternalRestartUnsupported { core: self.kind }),
        }
    }

    pub(super) fn validate_backup_delta(
        &self,
        delta: &serde_json::Value,
    ) -> Result<(), CoreSessionError> {
        // TUN preparation also includes the observed enable flag. Reserve its largest representation.
        let mut delta = delta.clone();
        if let Some(tun) = delta
            .get_mut("tun")
            .and_then(serde_json::Value::as_object_mut)
        {
            tun.entry("enable")
                .or_insert(serde_json::Value::Bool(false));
        }
        let pending = self.pending_backup.read().clone();
        if let Some(pending) = pending {
            let _ = snapshot_with_delta(&pending.snapshot, &delta)?;
        }
        Ok(())
    }

    pub(super) fn accept_saved_delta(
        &self,
        config: Option<CommittedConfig>,
        delta: Option<&serde_json::Value>,
    ) -> Result<u64, CoreSessionError> {
        let Some(delta) = delta else {
            return Ok(self.next_generation_with_config(config));
        };
        // The explicit receipt proves persistence, even when Commit acknowledgement is unknown.
        let previous_pending = self.pending_backup.read().clone();
        let updated_snapshot = previous_pending
            .as_ref()
            .map(|pending| snapshot_with_delta(&pending.snapshot, delta))
            .transpose()?;
        // YAML processing happens outside locks read by UI snapshots. The transition
        // guard serializes the authoritative backup target and accepted generation.
        let mut committed = self.committed_profile.write();
        let mut pending = self.pending_backup.write();
        if let (Some(pending), Some(snapshot)) = (pending.as_mut(), updated_snapshot) {
            pending.snapshot = snapshot;
        }
        if let Some(config) = config {
            committed.config = config;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        committed.generation = generation;
        if let Some(pending) = pending.as_mut() {
            pending.generation = generation;
        }
        self.client.invalidate_connections();
        Ok(generation)
    }
    pub(crate) async fn apply_prepared_local_config(
        &self,
        store: &ControlledConfigStore,
        prepared: Box<PreparedLocalConfig>,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let prepared = *prepared;
        if prepared.store_root != store.root()
            || !Arc::ptr_eq(&prepared.session_generation, &self.generation)
        {
            return Err(MihomoError::StaleBinding.into());
        }
        let client = self.client.pin_binding()?;
        self.verify_prepared_mihomo_runtime(&client, prepared.expected_runtime)?;
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mut committed = self.transition.clone().lock_owned().await;
        self.verify_prepared_mihomo_runtime(&client, prepared.expected_runtime)?;
        let session = self.clone();
        tokio::spawn(async move {
            let _availability_pause = session.pause_pac_availability();
            let _lease = lease;
            let (kind, receipt) = store
                .apply_frozen_local_update(
                    &client,
                    prepared.update,
                    prepared.expected_cache,
                    prepared.persist_patch,
                    session.shutdown_requested.clone(),
                )
                .await
                .map_err(|error| session.runtime_mutation_error(error))?;
            *committed = prepared.committed.clone();
            let generation = session.next_generation_with_config(Some(prepared.committed));
            receipt.confirmation?;
            Ok(CoreApplyOutcome { kind, generation })
        })
        .await
        .map_err(|error| {
            self.invalidate_runtime_observations();
            CoreSessionError::Config(ControlledConfigError::Task(error.to_string()))
        })?
    }

    pub(super) fn verify_prepared_mihomo_runtime(
        &self,
        client: &MihomoClient,
        expected: RuntimeConfigurationRevision,
    ) -> Result<(), CoreSessionError> {
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        if self.kind != CoreKind::Mihomo
            || self.runtime_descriptor().binding_generation() != expected.binding_generation
            || self.generation() != expected.configuration_generation
        {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
