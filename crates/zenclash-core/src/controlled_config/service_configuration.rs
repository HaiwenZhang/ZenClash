//! Service and ordinary-recovery candidate preparation with scoped persistence participants.

use super::*;

pub(crate) struct ServiceTunPersistence {
    cache: RuntimeCacheTransaction,
    store: ControlledConfigStore,
    update: ControlledConfigUpdate,
}

impl ServiceTunPersistence {
    pub(crate) async fn save(&self) -> ControlledConfigResult<()> {
        let store = self.store.clone();
        let update = self.update.clone();
        tokio::task::spawn_blocking(move || store.commit(&update))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
    pub(crate) async fn validate_patch(&self) -> ControlledConfigResult<()> {
        let store = self.store.clone();
        let expected = self.update.expected_patch.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            if store.current_patch_bytes_unlocked()? != expected {
                return Err(ControlledConfigError::ConcurrentModification);
            }
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        rollback_runtime_cache(self.cache).await
    }
    pub(crate) fn saved(self) {
        self.cache.commit();
    }
}

pub(crate) struct ServiceStartupPersistence(RuntimeCacheTransaction);

impl ServiceStartupPersistence {
    pub(crate) fn saved(self) {
        self.0.commit();
    }

    pub(crate) async fn rollback(self) -> ControlledConfigResult<()> {
        rollback_runtime_cache(self.0).await
    }
}

impl ControlledConfigStore {
    pub(crate) async fn lock_service_tun_mutation(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.mutation_gate.clone().lock_owned().await
    }

    pub(crate) fn owns_service_tun_mutation(
        &self,
        guard: &tokio::sync::OwnedMutexGuard<()>,
    ) -> bool {
        Arc::ptr_eq(
            tokio::sync::OwnedMutexGuard::mutex(guard),
            &self.mutation_gate,
        )
    }

    pub(crate) async fn prepare_service_tun_update(
        &self,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            if store.cached_runtime_payload()?.is_none() && bundle.is_none() {
                return Err(ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                )));
            }
            store.prepare_partial_update(
                &profile,
                &serde_json::json!({"tun":{"enable":true}}),
                &overrides,
                bundle.as_deref(),
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn validate_prepared_service_tun(
        &self,
        update: ControlledConfigUpdate,
        expected_cache: Option<String>,
    ) -> ControlledConfigResult<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            if store.current_patch_bytes_unlocked()? != update.expected_patch
                || store.cached_runtime_payload()? != expected_cache
            {
                return Err(ControlledConfigError::ConcurrentModification);
            }
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn prepare_service_config_update(
        &self,
        profile: impl Into<crate::profile::ProfileRuntimeSource> + Send,
        patch: Option<serde_json::Value>,
        overrides: Vec<PathBuf>,
        previous_payload: Option<String>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let profile = profile.into();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut update =
                if let (crate::profile::ProfileRuntimeSource::File(path), Some(patch)) =
                    (&profile, &patch)
                {
                    store.prepare_update(path, serde_yaml::to_value(patch)?)?
                } else {
                    if patch.is_some() {
                        return Err(ControlledConfigError::PartialOverrideConflict);
                    }
                    let _transaction = store.transaction.lock();
                    let (expected_patch, current) = store.load_unlocked()?;
                    let payload = match profile {
                        crate::profile::ProfileRuntimeSource::File(path) => {
                            merge_profile_patch(&path, current.clone())?
                        }
                        crate::profile::ProfileRuntimeSource::Frozen(payload) => {
                            crate::profile::merge_payload_patch(&payload, current.clone())?
                        }
                        crate::profile::ProfileRuntimeSource::Prepared(_) => {
                            return Err(ControlledConfigError::PartialOverrideConflict);
                        }
                    };
                    ControlledConfigUpdate {
                        expected_patch,
                        next_patch: serde_yaml::to_string(&current)?.into_bytes(),
                        previous_payload: payload.clone(),
                        next_payload: payload,
                    }
                };
            update.next_payload =
                store.apply_session_listener_fallbacks(&normalize_runtime_payload(
                    CoreKind::Mihomo,
                    merge_payload_overrides(update.next_payload(), &overrides)?,
                )?)?;
            if let Some(payload) = previous_payload {
                update.previous_payload = payload;
            }
            validate_listener_change(update.previous_payload(), update.next_payload(), true)
                .map_err(ControlledConfigError::ListenerFallback)?;
            Ok(update)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) fn prepare_backup_config_update(
        &self,
        candidate: &crate::backup::BackupRuntimeCandidate,
        previous_payload: String,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let next_payload = self.apply_session_listener_fallbacks(&normalize_runtime_payload(
            CoreKind::Mihomo,
            candidate.payload.clone(),
        )?)?;
        validate_listener_change(&previous_payload, &next_payload, true)
            .map_err(ControlledConfigError::ListenerFallback)?;
        Ok(ControlledConfigUpdate {
            expected_patch: Some(candidate.patch.clone()),
            next_patch: candidate.patch.clone(),
            previous_payload,
            next_payload,
        })
    }

    pub(crate) async fn prepare_service_tun_recovery_update(
        &self,
        bundle: Arc<crate::ServiceRuntimeBundle>,
    ) -> ControlledConfigResult<ControlledConfigUpdate> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _transaction = store.transaction.lock();
            let (expected_patch, current) = store.load_unlocked()?;
            Self::prepare_partial_payload_update(
                expected_patch,
                current,
                bundle.yaml().to_owned(),
                &serde_json::json!({"tun":{"enable":true}}),
                &[],
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    // Caller holds the store mutation gate; borrowed write authority must remain live.
    pub(crate) async fn persist_local_recovery_payload_admitted(
        &self,
        payload: String,
    ) -> ControlledConfigResult<()> {
        let lease = self
            .write_access
            .borrowed_authority(std::slice::from_ref(&self.root))
            .map_err(ControlledConfigError::Task)?
            .ok_or_else(|| {
                ControlledConfigError::Task("Local recovery write authority has expired".into())
            })?;
        let store = self.with_write_lease(&lease);
        tokio::spawn(async move {
            let _lease = lease;
            let prepare = store.clone();
            let update = tokio::task::spawn_blocking(move || {
                if payload.len() > MAX_PROFILE_BYTES {
                    return Err(ControlledConfigError::TooLarge);
                }
                let document: Value = serde_yaml::from_str(&payload)?;
                if document
                    .get("tun")
                    .and_then(|tun| tun.get("enable"))
                    .and_then(Value::as_bool)
                    != Some(false)
                {
                    return Err(ControlledConfigError::PartialOverrideConflict);
                }
                let _transaction = prepare.transaction.lock();
                let (expected, current) = prepare.load_unlocked()?;
                Self::prepare_partial_payload_update(
                    expected,
                    current,
                    payload,
                    &serde_json::json!({"tun":{"enable":false}}),
                    &[],
                )
            })
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
            let persistence = store.stage_service_tun_update(update).await?;
            if let Err(error) = persistence.save().await {
                return match persistence.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(ControlledConfigError::Transaction(format!(
                        "{error}; {rollback}"
                    ))),
                };
            }
            persistence.saved();
            Ok(())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn prepare_service_startup_payload(
        &self,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> ControlledConfigResult<(String, Vec<ListenerPortFallback>)> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let payload = store.effective_with_overrides(profile, &overrides)?;
            let payload = normalize_runtime_payload(CoreKind::Mihomo, payload)?;
            let payload = store.apply_session_listener_fallbacks(&payload)?;
            let mut document = serde_yaml::from_str::<Value>(&payload)?;
            let mut next_session = store.session_listener_fallbacks.lock().clone();
            let resolved = resolve_conflicts(&mut document, &mut next_session)
                .map_err(ControlledConfigError::ListenerFallback)?;
            let payload = serde_yaml::to_string(&document)?;
            if payload.len() > MAX_PROFILE_BYTES {
                return Err(ControlledConfigError::TooLarge);
            }
            *store.session_listener_fallbacks.lock() = next_session;
            Ok((
                payload,
                resolved
                    .into_iter()
                    .map(|(listener, fallback)| ListenerPortFallback {
                        listener,
                        original: fallback.original,
                        current: fallback.current,
                    })
                    .collect(),
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn stage_service_startup_payload(
        &self,
        payload: String,
    ) -> ControlledConfigResult<super::ServiceStartupPersistence> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            store
                .stage_runtime_payload(&payload)
                .map(ServiceStartupPersistence)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    pub(crate) async fn stage_service_tun_update(
        &self,
        update: ControlledConfigUpdate,
    ) -> ControlledConfigResult<super::ServiceTunPersistence> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let cache = store.stage_runtime_payload(&update.next_payload)?;
            Ok(ServiceTunPersistence {
                cache,
                store,
                update,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}
