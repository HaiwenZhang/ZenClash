//! Replays an accepted runtime payload verbatim, with its original restoration authority.

use super::runtime_application::{
    result_label, rollback_cache_and_restart, rollback_runtime_cache,
};
use super::{ControlledConfigError, ControlledConfigStore, RuntimeMutationError};
use crate::{ManagedCoreProcess, MihomoClient, MihomoError};
use std::sync::Arc;

impl ControlledConfigStore {
    pub(crate) async fn restore_exact_runtime_payload(
        &self,
        client: &MihomoClient,
        process: Option<Arc<ManagedCoreProcess>>,
        payload: String,
        service_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        authority: crate::core_session::RuntimeRestoreAuthority,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), RuntimeMutationError> {
        let client = client.pin_binding()?;
        let mut scopes = client.write_scopes();
        if let Some(process) = &process {
            scopes.extend(process.write_scopes());
        }
        let write_lease = self.acquire_write_lease_for_paths(scopes).await?;
        client.ensure_binding_current()?;
        let client = client.with_write_lease(&write_lease)?;
        let store = self.with_write_lease(&write_lease);
        let _mutation_guard = store.mutation_gate.lock().await;
        client.ensure_binding_current()?;
        // Validate before touching the cache, without remerging source files or fallbacks.
        let prepared = if process.is_none() {
            Some(
                client
                    .prepare_saved_runtime(payload.clone(), service_bundle, authority)
                    .await?,
            )
        } else {
            None
        };
        let worker_lease = store.write_access.acquire();
        let cache_store = store.with_write_lease(&worker_lease);
        let validator = process
            .as_ref()
            .map(|process| process.config_validator().with_write_lease(&worker_lease));
        let cache_payload = payload.clone();
        let cache = tokio::task::spawn_blocking(move || {
            let _write_lease = worker_lease;
            if let Some(validator) = validator {
                validator
                    .validate_payload(&cache_payload)
                    .map_err(|error| {
                        ControlledConfigError::Profile(MihomoError::Process(error.to_string()))
                    })?;
            }
            cache_store.stage_runtime_payload(&cache_payload)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let mut applied_runtime = None;
        client.ensure_binding_current()?;
        let (applied, attempted) = if let Some(process) = &process {
            let attempted = !cancelled.load(std::sync::atomic::Ordering::Acquire);
            (
                process
                    .restart_and_wait_until_with_lease(
                        std::time::Duration::from_secs(20),
                        Some(cancelled.clone()),
                        &write_lease,
                    )
                    .await,
                attempted,
            )
        } else {
            let applied = match prepared {
                Some(prepared) => prepared.apply(true).await.map(|runtime| {
                    applied_runtime = Some(runtime);
                }),
                None => Err(MihomoError::Process("Missing prepared runtime".into())),
            };
            let attempted = applied
                .as_ref()
                .err()
                .is_some_and(MihomoError::mutation_result_unknown);
            (applied, attempted)
        };
        if let Err(error) = applied {
            let cache_rollback = if let Some(process) = process {
                rollback_cache_and_restart(cache, process, &write_lease, Some(cancelled)).await
            } else {
                result_label(rollback_runtime_cache(cache).await)
            };
            return Err(RuntimeMutationError {
                attempted,
                cause: ControlledConfigError::Transaction(zenclash_i18n::text_with(
                    "backup.errors.exact_runtime_restore_failed",
                    &[("error", error.to_string()), ("cache", cache_rollback)],
                )),
            });
        }
        cache.commit();
        if let Some(runtime) = applied_runtime {
            runtime.commit().await.map_err(|cause| {
                RuntimeMutationError::attempted(ControlledConfigError::Profile(cause))
            })?;
        }
        Ok(())
    }
}
