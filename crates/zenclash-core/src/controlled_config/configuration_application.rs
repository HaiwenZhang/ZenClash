//! Full configuration acceptance, frozen fallback and durable controlled-patch completion.

use super::*;
use std::sync::atomic::AtomicBool;

// A policy for this operation, not another process or lifecycle owner.
enum ReloadFallback {
    Disabled,
    OwnedLocal { cancelled: Arc<AtomicBool> },
}

impl ControlledConfigStore {
    pub(crate) async fn apply_json_update_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<SavedRuntimeReceipt, RuntimeMutationError> {
        self.apply_json_update_with_policy(
            client,
            profile,
            patch,
            overrides,
            ReloadFallback::Disabled,
        )
        .await
        .map(|(_, receipt)| receipt)
    }

    pub(crate) async fn apply_json_update_with_fallback_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(crate::CoreApplyKind, SavedRuntimeReceipt), RuntimeMutationError> {
        self.apply_json_update_with_policy(
            client,
            profile,
            patch,
            overrides,
            ReloadFallback::OwnedLocal { cancelled },
        )
        .await
    }

    async fn apply_json_update_with_policy(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
        fallback: ReloadFallback,
    ) -> Result<(crate::CoreApplyKind, SavedRuntimeReceipt), RuntimeMutationError> {
        if client.service_client().is_some() && service_partial_patch(patch) {
            return self
                .apply_partial_update_for_session(client, profile, patch, overrides)
                .await
                .map(|receipt| (crate::CoreApplyKind::Patched, receipt));
        }
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let prepare_lease = leased_store.write_access.acquire();
        let prepare_store = leased_store.with_write_lease(&prepare_lease);
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let (update, applied_payload) = tokio::task::spawn_blocking(move || {
            let _write_lease = prepare_lease;
            let applied_payload = prepare_store.cached_runtime_payload()?;
            Ok::<_, ControlledConfigError>((
                prepare_store.prepare_json_update(profile, &patch)?,
                applied_payload,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_base = update.next_payload().to_owned();
        let previous_base = update.previous_payload().to_owned();
        let (next_runtime, previous_runtime) = tokio::task::spawn_blocking(move || {
            Ok::<_, MihomoError>((
                merge_payload_overrides(&next_base, &overrides)?,
                merge_payload_overrides(&previous_base, &overrides)?,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let previous_runtime = leased_store.apply_session_listener_fallbacks(&previous_runtime)?;
        let next_runtime = leased_store.apply_session_listener_fallbacks(&next_runtime)?;
        let previous_runtime = applied_payload.as_deref().unwrap_or(&previous_runtime);
        validate_listener_change(previous_runtime, &next_runtime, true)
            .map_err(ControlledConfigError::ListenerFallback)?;
        let (transaction, kind) = leased_store
            .accept_full_config(
                client,
                next_runtime,
                applied_payload,
                RuntimeMutationGuards::new(mutation_guard, write_lease),
                fallback,
            )
            .await?;
        // One admitted task retains the write lease, store gate and runtime token to completion.
        tokio::spawn(async move {
            let commit_lease = leased_store.write_access.acquire();
            let commit_store = leased_store.with_write_lease(&commit_lease);
            let commit_update = update.clone();
            let commit = tokio::task::spawn_blocking(move || {
                let _write_lease = commit_lease;
                commit_store.commit(&commit_update)
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
            .and_then(|result| result);
            if let Err(error) = commit {
                return match transaction.rollback().await {
                    Ok(()) => Err(ControlledConfigError::Transaction(format!(
                        "保存失败，启动缓存与 Mihomo 均已恢复上一版本：{error}"
                    ))),
                    Err(rollback) => Err(ControlledConfigError::Transaction(format!(
                        "保存失败：{error}；上一版本恢复：{rollback}"
                    ))),
                }
                .map_err(RuntimeMutationError::attempted);
            }
            Ok((
                kind,
                SavedRuntimeReceipt {
                    delta: None,
                    confirmation: transaction.commit().await,
                },
            ))
        })
        .await
        .map_err(|error| {
            RuntimeMutationError::attempted(ControlledConfigError::Task(error.to_string()))
        })?
    }

    // The session owns completion and its transition gate; this method keeps
    // persistence, restart fallback and rollback on the same checked payload.
    pub(crate) async fn apply_frozen_local_update(
        &self,
        client: &MihomoClient,
        update: ControlledConfigUpdate,
        expected_cache: Option<String>,
        persist_patch: bool,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(crate::CoreApplyKind, SavedRuntimeReceipt), RuntimeMutationError> {
        let lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = self.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let mutation = store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        store
            .validate_prepared_service_tun(update.clone(), expected_cache)
            .await?;
        let payload = update.next_payload().to_owned();
        let previous_payload = store.cached_runtime_payload()?;
        let (transaction, kind) = store
            .accept_full_config(
                &client,
                payload,
                previous_payload,
                RuntimeMutationGuards::new(mutation, lease),
                ReloadFallback::OwnedLocal { cancelled },
            )
            .await?;
        if persist_patch {
            let commit_lease = store.write_access.acquire();
            let commit_store = store.with_write_lease(&commit_lease);
            let commit = tokio::task::spawn_blocking(move || {
                let _lease = commit_lease;
                commit_store.commit(&update)
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))
            .and_then(|result| result);
            if let Err(error) = commit {
                let rollback = transaction.rollback().await;
                return Err(RuntimeMutationError::attempted(
                    ControlledConfigError::Transaction(format!(
                        "保存失败：{error}；上一版本恢复：{}",
                        result_label(rollback)
                    )),
                ));
            }
        }
        Ok((
            kind,
            SavedRuntimeReceipt {
                delta: None,
                confirmation: transaction.commit().await,
            },
        ))
    }

    pub(crate) async fn stage_profile_reload(
        &self,
        client: &MihomoClient,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        _previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
    ) -> Result<RuntimeApplicationTransaction, RuntimeMutationError> {
        self.stage_profile_runtime(
            client,
            candidate.into(),
            overrides,
            ReloadFallback::Disabled,
        )
        .await
        .map(|(transaction, _)| transaction)
    }

    pub(crate) async fn stage_profile_reload_with_restart(
        &self,
        client: &MihomoClient,
        candidate: crate::profile::ProfileRuntimeSource,
        overrides: Vec<PathBuf>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(RuntimeApplicationTransaction, crate::CoreApplyKind), RuntimeMutationError> {
        self.stage_profile_runtime(
            client,
            candidate,
            overrides,
            ReloadFallback::OwnedLocal { cancelled },
        )
        .await
    }

    pub(crate) async fn reload_with_fallback_for_session(
        &self,
        client: &MihomoClient,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<crate::CoreApplyKind, RuntimeMutationError> {
        let (transaction, kind) = self
            .stage_profile_reload_with_restart(
                client,
                crate::profile::ProfileRuntimeSource::File(profile),
                overrides,
                cancelled,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(RuntimeMutationError::attempted)?;
        Ok(kind)
    }

    async fn stage_profile_runtime(
        &self,
        client: &MihomoClient,
        candidate: crate::profile::ProfileRuntimeSource,
        overrides: Vec<PathBuf>,
        fallback: ReloadFallback,
    ) -> Result<(RuntimeApplicationTransaction, crate::CoreApplyKind), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let client = client.with_write_lease(&write_lease)?;
        let store = self.with_write_lease(&write_lease);
        let mutation_guard = store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let worker_lease = store.write_access.acquire();
        let worker = store.with_write_lease(&worker_lease);
        let (payload, previous_payload) = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            let previous_payload = worker.cached_runtime_payload()?;
            Ok::<_, ControlledConfigError>((
                worker.effective_source_with_overrides(candidate, &overrides)?,
                previous_payload,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload =
            store.validate_candidate_listeners(client.normalize_config_payload(payload)?, true)?;
        store
            .accept_full_config(
                &client,
                payload,
                previous_payload,
                RuntimeMutationGuards::new(mutation_guard, write_lease),
                fallback,
            )
            .await
    }

    pub(crate) async fn stage_profile_validation(
        &self,
        kind: CoreKind,
        client: &MihomoClient,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<RuntimeCandidateValidation> {
        let candidate = candidate.into();
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        client.ensure_binding_current()?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_source_with_overrides(candidate, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(kind, payload)?;
        client.validate_config_payload(&payload).await?;
        Ok(RuntimeCandidateValidation::new(RuntimeMutationGuards::new(
            mutation_guard,
            write_lease,
        )))
    }

    // Full updates and staged catalog changes share the same frozen fallback.
    // The caller decides when business persistence permits commit or rollback.
    async fn accept_full_config(
        &self,
        client: &MihomoClient,
        payload: String,
        previous_payload: Option<String>,
        guards: RuntimeMutationGuards,
        fallback: ReloadFallback,
    ) -> Result<(RuntimeApplicationTransaction, crate::CoreApplyKind), RuntimeMutationError> {
        match self
            .accept_runtime_payload_with_snapshot(client, payload.clone(), previous_payload)
            .await
        {
            Ok(accepted) => Ok((
                RuntimeApplicationTransaction::hot_reload(accepted, guards),
                crate::CoreApplyKind::HotReloaded,
            )),
            Err(error) => {
                let ReloadFallback::OwnedLocal { cancelled } = fallback else {
                    return Err(error);
                };
                if !crate::core_session::should_restart_after_hot_reload(&error.cause) {
                    return Err(error);
                }
                client
                    .ensure_binding_current()
                    .map_err(|cause| RuntimeMutationError {
                        cause: cause.into(),
                        attempted: error.attempted,
                    })?;
                let Some(crate::owned_core::OwnedCore::Local(process)) = client.owned_core() else {
                    return Err(error);
                };
                // An owned restart reads its launch path. It must consume this
                // generated cache, never a mutable source beside the candidate.
                if process.launch_config().config_file != self.runtime_path() {
                    return Err(error);
                }
                let cache = self
                    .accept_runtime_payload_with_restart_for_session(
                        process.clone(),
                        payload,
                        Some(cancelled.clone()),
                    )
                    .await
                    .map_err(|mut restart| {
                        restart.attempted |= error.attempted;
                        restart
                    })?;
                Ok((
                    RuntimeApplicationTransaction::restart(cache, process, Some(cancelled), guards),
                    crate::CoreApplyKind::Restarted,
                ))
            }
        }
    }

    pub(super) async fn accept_runtime_payload(
        &self,
        client: &MihomoClient,
        payload: String,
    ) -> ControlledConfigResult<AcceptedRuntime> {
        self.accept_runtime_payload_for_session(client, payload)
            .await
            .map_err(|error| error.cause)
    }

    pub(super) async fn accept_runtime_payload_for_session(
        &self,
        client: &MihomoClient,
        payload: String,
    ) -> Result<AcceptedRuntime, RuntimeMutationError> {
        let previous_payload = self.cached_runtime_payload()?;
        self.accept_runtime_payload_with_snapshot(client, payload, previous_payload)
            .await
    }

    async fn accept_runtime_payload_with_snapshot(
        &self,
        client: &MihomoClient,
        payload: String,
        previous_payload: Option<String>,
    ) -> Result<AcceptedRuntime, RuntimeMutationError> {
        let payload = client.normalize_config_payload(payload)?;
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let prepared = client.prepare_runtime_payload(payload.clone()).await?;
        let worker_lease = self.write_access.acquire();
        let store = self.with_write_lease(&worker_lease);
        let cache_payload = payload.clone();
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.stage_runtime_payload(&cache_payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let runtime = match prepared.apply(true).await {
            Ok(runtime) => runtime,
            Err(error) => {
                let attempted = error.mutation_result_unknown();
                return match tokio::task::spawn_blocking(move || cache.rollback()).await {
                    Ok(Ok(())) => Err(ControlledConfigError::Profile(error)),
                    Ok(Err(rollback)) => Err(ControlledConfigError::Transaction(format!(
                        "Mihomo 拒绝配置：{error}；恢复启动缓存失败：{rollback}"
                    ))),
                    Err(task) => Err(ControlledConfigError::Task(format!(
                        "Mihomo 拒绝配置：{error}；缓存恢复任务异常结束：{task}"
                    ))),
                }
                .map_err(|cause| RuntimeMutationError { cause, attempted });
            }
        };
        Ok(AcceptedRuntime::new(cache, runtime, previous_payload))
    }

    pub(super) async fn accept_runtime_payload_with_restart_for_session(
        &self,
        process: Arc<ManagedCoreProcess>,
        payload: String,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<RuntimeCacheTransaction, RuntimeMutationError> {
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = self.apply_session_listener_fallbacks(&payload)?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let validator = process.config_validator().with_write_lease(&worker_lease);
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            validator.validate_payload(&payload).map_err(|error| {
                ControlledConfigError::Profile(MihomoError::Process(error.to_string()))
            })?;
            store.stage_runtime_payload(&payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let attempted = cancelled
            .as_ref()
            .is_none_or(|flag| !flag.load(std::sync::atomic::Ordering::Acquire));
        if let Err(error) = process
            .restart_and_wait_until_with_lease(
                std::time::Duration::from_secs(20),
                cancelled.clone(),
                &write_lease,
            )
            .await
        {
            let rollback =
                rollback_cache_and_restart(cache, process, &write_lease, cancelled).await;
            return Err(RuntimeMutationError {
                attempted,
                cause: ControlledConfigError::Transaction(format!(
                    "新配置启动失败：{error}；上一配置恢复：{rollback}"
                )),
            });
        }
        Ok(cache)
    }
    /// Applies, verifies, and persists a JSON override through real Mihomo.
    ///
    /// The startup cache is staged before the complete effective YAML is sent
    /// to Mihomo. The override and cache are committed together only after
    /// Mihomo accepts the payload; failures restore both runtime and cache.
    ///
    /// # Errors
    ///
    /// Returns preparation, Mihomo reload, commit, rollback, or task errors.
    pub async fn apply_json_update(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
    ) -> ControlledConfigResult<()> {
        self.apply_json_update_with_overrides(client, profile, patch, Vec::new())
            .await
    }

    /// Applies and persists a JSON patch while preserving ordered YAML overrides.
    ///
    /// The candidate includes the current explicit override chain. On failure,
    /// rollback replays the cached payload from the previous application.
    ///
    /// # Errors
    ///
    /// Returns preparation, merge, Mihomo reload, commit, rollback, or task errors.
    pub async fn apply_json_update_with_overrides(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.apply_json_update_for_session(client, profile, patch, overrides)
            .await
            .map_err(|error| error.cause)?
            .confirmation
    }

    /// Applies and persists a JSON patch by restarting a managed core with the
    /// newly materialized effective profile.
    ///
    /// # Errors
    ///
    /// Returns preparation, merge, restart, readiness, commit, or rollback errors.
    pub async fn apply_json_update_with_restart(
        &self,
        process: Arc<ManagedCoreProcess>,
        profile: impl AsRef<Path>,
        patch: &serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        let profile = profile.as_ref().to_path_buf();
        let patch = patch.clone();
        let application = AdmittedManagedRestart::acquire(self, process)
            .await
            .map_err(|error| error.cause)?;
        tokio::spawn(application.apply_patch(profile, patch, overrides, None))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?
            .map_err(|error| error.cause)
    }

    /// Reloads one source profile with the persisted controlled layer applied.
    ///
    /// This operation shares the same asynchronous mutation gate as
    /// [`Self::apply_json_update`], preventing a Profile switch from racing a
    /// settings update.
    ///
    /// # Errors
    ///
    /// Returns merge, task, or Mihomo reload errors.
    pub async fn reload_profile(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
    ) -> ControlledConfigResult<()> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let store = leased_store.clone();
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || store.effective_payload(profile))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = leased_store.validate_candidate_listeners(payload, true)?;
        leased_store
            .accept_runtime_payload(client, payload)
            .await?
            .commit()
            .await?;
        Ok(())
    }

    /// Reloads a profile with both the controlled layer and ordered YAML
    /// override files applied.
    ///
    /// The controlled layer is merged first, followed by the explicit files
    /// in their supplied order. This shares the profile mutation gate so a
    /// settings save cannot race the reload.
    ///
    /// # Errors
    ///
    /// Returns merge, task, file-read, or Mihomo reload errors.
    pub async fn reload_with_overrides(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        self.reload_with_overrides_for_session(client, profile, overrides)
            .await
            .map_err(|error| error.cause)
    }

    pub(crate) async fn reload_with_overrides_for_session(
        &self,
        client: &MihomoClient,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> Result<(), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let write_lease = self
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        client.ensure_binding_current()?;
        let leased_client = client.with_write_lease(&write_lease)?;
        let client = &leased_client;
        let leased_store = self.with_write_lease(&write_lease);
        let _mutation_guard = leased_store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let profile = profile.as_ref().to_path_buf();
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = leased_store.validate_candidate_listeners(payload, true)?;
        leased_store
            .accept_runtime_payload_for_session(client, payload)
            .await?
            .commit()
            .await
            .map_err(RuntimeMutationError::attempted)?;
        Ok(())
    }

    /// Materializes a complete effective profile and restarts a managed core.
    ///
    /// This provides real profile switching for cores whose hot-reload API is
    /// incomplete. A failed restart restores the prior runtime cache and
    /// attempts to bring the previous configuration back online. Cancellation while
    /// queued has no effects; admitted completion survives a dropped foreground waiter.
    ///
    /// # Errors
    ///
    /// Returns merge, cache, process restart, readiness, or rollback errors.
    pub async fn restart_with_overrides(
        &self,
        process: Arc<ManagedCoreProcess>,
        profile: impl AsRef<Path>,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<()> {
        let profile = profile.as_ref().to_path_buf();
        let application = AdmittedManagedRestart::acquire(self, process)
            .await
            .map_err(|error| error.cause)?;
        tokio::spawn(application.restart_profile(profile, overrides, None))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?
            .map_err(|error| error.cause)
    }

    pub(crate) async fn stage_profile_restart(
        &self,
        process: Arc<ManagedCoreProcess>,
        candidate: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<RuntimeApplicationTransaction, RuntimeMutationError> {
        let candidate = candidate.into();
        let write_lease = self
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let leased_store = self.with_write_lease(&write_lease);
        let mutation_guard = leased_store.mutation_gate.clone().lock_owned().await;
        let worker_lease = leased_store.write_access.acquire();
        let store = leased_store.with_write_lease(&worker_lease);
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_source_with_overrides(candidate, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(process.kind(), payload)?;
        let payload = leased_store.validate_candidate_listeners(payload, process.is_running())?;
        let cache = leased_store
            .accept_runtime_payload_with_restart_for_session(
                process.clone(),
                payload,
                cancelled.clone(),
            )
            .await?;
        Ok(RuntimeApplicationTransaction::restart(
            cache,
            process,
            cancelled,
            RuntimeMutationGuards::new(mutation_guard, write_lease),
        ))
    }
}
