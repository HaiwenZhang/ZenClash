//! First runtime application on an already-owned service binding.

use super::*;
use crate::service_runtime_session::{RuntimeSession, RuntimeTransport};

/// Startup facts retained independently of service acknowledgement failures.
#[derive(Debug)]
pub struct CoreInitializationOutcome {
    saved: Option<CoreApplyOutcome>,
    failure: Option<CoreSessionError>,
    commit_pending: bool,
    listener_fallbacks: Vec<crate::ListenerPortFallback>,
}

impl CoreInitializationOutcome {
    /// Returns a receipt only after the generated startup cache was durably accepted.
    #[must_use]
    pub const fn saved(&self) -> Option<&CoreApplyOutcome> {
        self.saved.as_ref()
    }

    /// Returns the failure without discarding a successful persistence receipt.
    #[must_use]
    pub const fn failure(&self) -> Option<&CoreSessionError> {
        self.failure.as_ref()
    }

    /// Reports a saved candidate awaiting native Commit confirmation.
    #[must_use]
    pub const fn commit_pending(&self) -> bool {
        self.commit_pending
    }

    /// Returns session-only listener changes applied before service validation.
    #[must_use]
    pub fn listener_fallbacks(&self) -> &[crate::ListenerPortFallback] {
        &self.listener_fallbacks
    }
}

impl CoreSession {
    /// Initializes the first configuration on this session's verified service owner.
    ///
    /// Construct the session with [`Self::open`] before calling this method. The
    /// same owner remains reachable for shutdown after an uncertain Start. Once
    /// admitted, cancellation of the waiter does not cancel persistence or cleanup.
    /// No local executable is discovered, validated or started.
    ///
    /// # Errors
    /// Rejects non-service bindings, an already initialized session, stale bindings,
    /// shutdown or failed preparation. Failures after a saved cache are returned
    /// inside the outcome and never erase its persistence receipt.
    pub async fn initialize_service_runtime(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreInitializationOutcome, CoreSessionError> {
        let client = self.client.pin_binding()?;
        let runtime = client
            .runtime_session()
            .ok_or(CoreSessionError::ReleaseUnsupported { core: self.kind })?;
        let lease = store
            .acquire_write_lease_for_paths(self.write_scopes_for_client(&client))
            .await?;
        client.ensure_binding_current()?;
        let store = store.with_write_lease(&lease);
        let client = client.with_write_lease(&lease)?;
        let committed = self.transition.clone().lock_owned().await;
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        if committed.profile.is_some()
            || self.lifecycle_snapshot().phase == CoreLifecyclePhase::Unknown
        {
            return Err(MihomoError::InvalidInput(zenclash_i18n::text(
                "core_page.service.unknown",
            ))
            .into());
        }
        let session = self.clone();
        self.complete_service_initialization(async move {
            let _lease = lease;
            let _store_mutation = store.lock_service_tun_mutation().await;
            client.ensure_binding_current()?;
            let _mutation = client.lock_runtime_binding().await?;
            session.ensure_running_operations_allowed()?;
            let readback = async { client.runtime_config().await.map(|_| ()) };
            session
                .initialize_service_admitted(
                    &store,
                    runtime,
                    committed,
                    CommittedConfig {
                        profile: Some(profile),
                        overrides,
                    },
                    readback,
                )
                .await
        })
        .await
    }

    async fn complete_service_initialization(
        &self,
        completion: impl std::future::Future<
            Output = Result<CoreInitializationOutcome, CoreSessionError>,
        > + Send
        + 'static,
    ) -> Result<CoreInitializationOutcome, CoreSessionError> {
        tokio::spawn(completion).await.map_err(|_| {
            self.lifecycle.write().phase = CoreLifecyclePhase::Unknown;
            ControlledConfigError::Task(zenclash_i18n::text("core_page.service.unknown"))
        })?
    }

    async fn initialize_service_admitted<T: RuntimeTransport>(
        &self,
        store: &ControlledConfigStore,
        runtime: Arc<RuntimeSession<T>>,
        mut committed: tokio::sync::OwnedMutexGuard<CommittedConfig>,
        next: CommittedConfig,
        readback: impl std::future::Future<Output = crate::MihomoResult<()>> + Send,
    ) -> Result<CoreInitializationOutcome, CoreSessionError> {
        let profile = next
            .profile
            .clone()
            .ok_or(CoreSessionError::NoCommittedProfile)?;
        let (payload, listener_fallbacks) = store
            .prepare_service_startup_payload(profile, next.overrides.clone())
            .await?;
        let prepared = runtime.prepare(&payload).await?;
        self.ensure_not_shutting_down()?;
        let cache = store.stage_service_startup_payload(payload).await?;
        if let Err(error) = self.ensure_not_shutting_down() {
            drop(prepared);
            cache.rollback().await?;
            return Err(error);
        }
        let applied = match prepared.apply(true).await {
            Ok(applied) => applied,
            Err(error) => {
                self.lifecycle.write().phase = if error.mutation_result_unknown() {
                    CoreLifecyclePhase::Unknown
                } else {
                    CoreLifecyclePhase::Failed
                };
                cache.rollback().await?;
                return Ok(CoreInitializationOutcome {
                    saved: None,
                    failure: Some(error.into()),
                    commit_pending: false,
                    listener_fallbacks,
                });
            }
        };
        if let Err(error) = readback
            .await
            .map_err(CoreSessionError::from)
            .and_then(|()| self.ensure_not_shutting_down())
        {
            drop(applied);
            let cache = cache.rollback().await;
            let stopped = runtime.stop_confirmed().await;
            self.lifecycle.write().phase = if stopped.is_ok() {
                CoreLifecyclePhase::Stopped
            } else {
                CoreLifecyclePhase::Unknown
            };
            cache?;
            return Ok(CoreInitializationOutcome {
                saved: None,
                failure: Some(error),
                commit_pending: false,
                listener_fallbacks,
            });
        }
        cache.saved();
        *committed = next.clone();
        let generation = self.next_generation_with_config(Some(next));
        // The cache and source identity are accepted. The exact bundle remains
        // with the same native candidate while Commit is pending.
        let confirmation = applied.commit().await;
        let commit_pending = confirmation.is_err();
        self.lifecycle.write().phase = if commit_pending {
            CoreLifecyclePhase::Unknown
        } else {
            CoreLifecyclePhase::Stable
        };
        Ok(CoreInitializationOutcome {
            saved: Some(CoreApplyOutcome {
                kind: CoreApplyKind::Restarted,
                generation,
            }),
            failure: confirmation.err().map(CoreSessionError::from),
            commit_pending,
            listener_fallbacks,
        })
    }
}

#[cfg(test)]
mod tests;
