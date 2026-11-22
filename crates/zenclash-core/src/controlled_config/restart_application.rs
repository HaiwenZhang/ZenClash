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
        let kind = self.process.kind();
        let running = self.process.is_running();
        let (update, next_runtime) = tokio::task::spawn_blocking(move || {
            let _write_lease = prepare_lease;
            let applied_payload = prepare_store.cached_runtime_payload()?;
            let update = prepare_store.prepare_json_update(profile, &patch)?;
            let next_runtime = normalize_runtime_payload(
                kind,
                merge_payload_overrides(update.next_payload(), &overrides)?,
            )?;
            let previous_runtime = normalize_runtime_payload(
                kind,
                merge_payload_overrides(update.previous_payload(), &overrides)?,
            )?;
            let next_runtime = prepare_store.apply_session_listener_fallbacks(&next_runtime)?;
            let previous_runtime =
                prepare_store.apply_session_listener_fallbacks(&previous_runtime)?;
            let previous_runtime = applied_payload.as_deref().unwrap_or(&previous_runtime);
            validate_listener_change(previous_runtime, &next_runtime, running)
                .map_err(ControlledConfigError::ListenerFallback)?;
            Ok::<_, ControlledConfigError>((update, next_runtime))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let cache = self
            .store
            .accept_runtime_payload_with_restart_for_session(
                self.process.clone(),
                next_runtime,
                None,
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
        let kind = self.process.kind();
        let running = self.process.is_running();
        let (payload, next_session) = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            let payload = store.effective_with_overrides(profile, &overrides)?;
            let payload = normalize_runtime_payload(kind, payload)?;
            store.resolve_candidate_listeners(payload, running)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        self.store
            .accept_runtime_payload_with_restart_for_session(
                self.process.clone(),
                payload,
                Some(next_session),
                cancelled,
            )
            .await?
            .commit();
        Ok(())
    }
}
