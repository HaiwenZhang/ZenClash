//! Admitted managed restart workflows. Public waiters and CoreSession use the same implementation.

use super::runtime_application::{RuntimeMutationGuards, rollback_cache_and_restart};
use super::{
    ControlledConfigError, ControlledConfigStore, RuntimeMutationError, normalize_runtime_payload,
};
use crate::{
    ManagedCoreProcess, listener_fallback::validate_listener_change,
    profile::merge_payload_overrides,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

/// Holds the existing process participant and mutation/data authority through completion.
/// It does not supervise, replace ownership or publish CoreSession generations.
pub(super) struct AdmittedManagedRestart {
    // Release authority before the last process Arc: its Drop can acquire the same scopes.
    guards: RuntimeMutationGuards,
    store: ControlledConfigStore,
    process: Arc<ManagedCoreProcess>,
}

impl AdmittedManagedRestart {
    pub(super) async fn acquire(
        store: &ControlledConfigStore,
        process: Arc<ManagedCoreProcess>,
    ) -> Result<Self, RuntimeMutationError> {
        let lease = store
            .acquire_write_lease_for_paths(process.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let mutation = store.mutation_gate.clone().lock_owned().await;
        Ok(Self {
            guards: RuntimeMutationGuards::new(mutation, lease),
            store,
            process,
        })
    }

    pub(super) async fn apply_patch(
        self,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Result<(), RuntimeMutationError> {
        let prepare_lease = self.store.write_access.acquire()?;
        let prepare_store = self.store.with_write_lease(&prepare_lease);
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
        let kind = self.process.kind();
        let (next_runtime, previous_runtime) = tokio::task::spawn_blocking(move || {
            Ok::<_, ControlledConfigError>((
                normalize_runtime_payload(kind, merge_payload_overrides(&next_base, &overrides)?)?,
                normalize_runtime_payload(
                    kind,
                    merge_payload_overrides(&previous_base, &overrides)?,
                )?,
            ))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let next_runtime = self.store.apply_session_listener_fallbacks(&next_runtime)?;
        let previous_runtime = self
            .store
            .apply_session_listener_fallbacks(&previous_runtime)?;
        let previous_runtime = applied_payload.as_deref().unwrap_or(&previous_runtime);
        validate_listener_change(previous_runtime, &next_runtime, self.process.is_running())
            .map_err(ControlledConfigError::ListenerFallback)?;
        let cache = self
            .store
            .accept_runtime_payload_with_restart_for_session(
                self.process.clone(),
                next_runtime,
                cancelled.clone(),
            )
            .await?;
        let commit_lease = self.store.write_access.acquire()?;
        let commit_store = self.store.with_write_lease(&commit_lease);
        let commit_update = update.clone();
        let commit = tokio::task::spawn_blocking(move || {
            let _write_lease = commit_lease;
            commit_store.commit(&commit_update)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))
        .and_then(|result| result);
        if let Err(error) = commit {
            let rollback = rollback_cache_and_restart(
                cache,
                self.process.clone(),
                self.guards.write_lease(),
                cancelled,
            )
            .await;
            return Err(RuntimeMutationError::attempted(
                ControlledConfigError::Transaction(format!(
                    "保存失败：{error}；运行内核恢复：{rollback}"
                )),
            ));
        }
        cache.commit();
        Ok(())
    }

    pub(super) async fn restart_profile(
        self,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Result<(), RuntimeMutationError> {
        let worker_lease = self.store.write_access.acquire()?;
        let store = self.store.with_write_lease(&worker_lease);
        let payload = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            store.effective_with_overrides(profile, &overrides)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let payload = normalize_runtime_payload(self.process.kind(), payload)?;
        let payload = self
            .store
            .validate_candidate_listeners(payload, self.process.is_running())?;
        self.store
            .accept_runtime_payload_with_restart_for_session(
                self.process.clone(),
                payload,
                cancelled,
            )
            .await?
            .commit();
        Ok(())
    }
}
