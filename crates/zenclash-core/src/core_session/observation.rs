//! Opaque identities for ordinary reads; they do not authorize mutations or recovery.

use std::sync::{Arc, Weak};

use parking_lot::RwLock;

use super::{CommittedConfig, CommittedProfileCache, CoreSession, CoreSessionError};
use crate::{MihomoClient, MihomoError, client::ControllerBinding};

/// A consistent runtime/configuration identity and its pinned read client.
#[derive(Clone)]
pub struct CoreObservation {
    client: MihomoClient,
    identity: CoreObservationIdentity,
}

/// Observation validity without retaining a retired core process or service owner.
#[derive(Clone)]
pub struct CoreObservationIdentity {
    read_identity: Arc<()>,
    binding: Weak<ControllerBinding>,
    binding_generation: u64,
    transition: Weak<tokio::sync::Mutex<CommittedConfig>>,
    data_application: Weak<tokio::sync::Mutex<()>>,
    shutdown_requested: Weak<std::sync::atomic::AtomicBool>,
    committed_profile: Weak<RwLock<CommittedProfileCache>>,
    configuration_generation: u64,
}

impl std::fmt::Debug for CoreObservationIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreObservationIdentity")
            .field("binding_generation", &self.binding_generation)
            .field("configuration_generation", &self.configuration_generation)
            .finish_non_exhaustive()
    }
}

impl CoreSession {
    /// Captures a read identity after any admitted runtime transition completes.
    /// No transition/data authority remains held during the network read.
    ///
    /// # Errors
    /// Returns an exit or stale-binding error when ordinary observation is unavailable.
    pub async fn begin_observation(&self) -> Result<CoreObservation, CoreSessionError> {
        let _data_application = self.data_application_gate.lock().await;
        let _transition = self.transition.lock().await;
        self.ensure_not_shutting_down()?;
        let client = self.client.pin_binding()?;
        let binding_generation = client.with_current_binding(|generation| generation)?;
        let configuration_generation = self.generation();
        Ok(CoreObservation {
            identity: CoreObservationIdentity {
                read_identity: Arc::new(()),
                binding: Arc::downgrade(&client.binding),
                binding_generation,
                transition: Arc::downgrade(&self.transition),
                data_application: Arc::downgrade(&self.data_application_gate),
                shutdown_requested: Arc::downgrade(&self.shutdown_requested),
                committed_profile: Arc::downgrade(&self.committed_profile),
                configuration_generation,
            },
            client,
        })
    }

    /// Captures ordinary read facts for a previously selected runtime client.
    ///
    /// A pinned client keeps its original binding. An unrelated client, a retired
    /// runtime or a changed observation is rejected instead of following a replacement.
    /// This observation does not authorize mutations or recovery.
    ///
    /// # Errors
    /// Returns shutdown or stale-binding errors when the supplied runtime cannot be observed.
    pub async fn begin_observation_for_runtime(
        &self,
        client: &MihomoClient,
    ) -> Result<CoreObservation, CoreSessionError> {
        let client = client.pin_binding()?;
        let observation = self.begin_observation().await?;
        observation
            .identity
            .publish_for_client(&client, |_| ())
            .ok_or(MihomoError::StaleBinding)?;
        client.ensure_binding_current()?;
        Ok(CoreObservation {
            client,
            identity: observation.identity,
        })
    }
}

impl CoreSession {
    /// Applies a compiled-rule flag only in the configuration that produced its index.
    /// The admitted patch owns the transition through acknowledgement, even when
    /// its foreground waiter leaves; configuration replacement cannot cross it.
    ///
    /// # Errors
    /// Returns stale observation, shutdown, authority, controller or task errors.
    pub async fn apply_observed_rule_disabled(
        &self,
        identity: &CoreObservationIdentity,
        index: usize,
        disabled: bool,
    ) -> Result<(), CoreSessionError> {
        if !identity.belongs_to(self) {
            return Err(MihomoError::StaleBinding.into());
        }
        let client = self.client.pin_binding()?;
        let lease = crate::data_coordinator::DataWriteLease::shared_async(client.write_scopes())
            .await
            .map_err(|error| super::ControlledConfigError::Task(error.to_string()))?;
        let transition = self.transition.clone().lock_owned().await;
        self.ensure_running_operations_allowed()?;
        let matches = client.with_current_binding(|binding_generation| {
            let cached = self.committed_profile.read();
            binding_generation == identity.binding_generation
                && cached.generation == identity.configuration_generation
        })?;
        if !matches {
            return Err(MihomoError::StaleBinding.into());
        }
        tokio::spawn(async move {
            let _lease = lease;
            let _transition = transition;
            client
                .apply_rule_disabled(index, disabled)
                .await
                .map_err(CoreSessionError::from)
        })
        .await
        .map_err(|error| super::ControlledConfigError::Task(error.to_string()))?
    }
}

impl CoreObservation {
    /// Waits for ordinary read eligibility while preserving this exact captured source.
    /// No observation guards or write authority are retained after this call.
    ///
    /// # Errors
    /// Returns stale binding for a replaced runtime/configuration, shutdown or dead owner.
    pub async fn wait_for_read_eligibility(&self) -> Result<(), MihomoError> {
        self.client.ensure_binding_current()?;
        self.identity.wait_for_read_eligibility().await?;
        self.client.ensure_binding_current()
    }
    /// Returns the pinned client for every request in this observation.
    #[must_use]
    pub fn client(&self) -> &MihomoClient {
        &self.client
    }
    /// Returns the lightweight identity used for publication and cache validity.
    #[must_use]
    pub fn identity(&self) -> &CoreObservationIdentity {
        &self.identity
    }
}

impl CoreObservationIdentity {
    /// Called only after the owning session has verified the client under mutation admission.
    pub(super) fn retain_admitted_read(&self, client: MihomoClient) -> CoreObservation {
        CoreObservation {
            client,
            identity: self.clone(),
        }
    }
    /// Waits out admitted data/transition work before validating an ordinary read.
    /// All guards are dropped before the caller performs I/O; no mutation authority is returned.
    pub(crate) async fn wait_for_read_eligibility(&self) -> Result<(), MihomoError> {
        let data_application = self
            .data_application
            .upgrade()
            .ok_or(MihomoError::StaleBinding)?;
        let transition = self.transition.upgrade().ok_or(MihomoError::StaleBinding)?;
        let _data_application = data_application.lock().await;
        let _transition = transition.lock().await;
        let shutdown = self
            .shutdown_requested
            .upgrade()
            .ok_or(MihomoError::StaleBinding)?;
        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
            return Err(MihomoError::StaleBinding);
        }
        let binding = self.binding.upgrade().ok_or(MihomoError::StaleBinding)?;
        let committed = self
            .committed_profile
            .upgrade()
            .ok_or(MihomoError::StaleBinding)?;
        let matches = binding.with_current_generation(self.binding_generation, || {
            committed.read().generation == self.configuration_generation
        })?;
        if matches {
            Ok(())
        } else {
            Err(MihomoError::StaleBinding)
        }
    }

    /// Returns whether both identities refer to the same captured read.
    /// Clones retain this identity; separate reads stay distinct even in one configuration.
    #[must_use]
    pub fn is_same_observation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.read_identity, &other.read_identity)
    }

    /// Checks source identity while the caller holds this session's transition and write lease.
    pub(super) fn verify_admitted_runtime(
        &self,
        session: &CoreSession,
        client: &MihomoClient,
    ) -> Result<(), MihomoError> {
        if !self.belongs_to(session)
            || !Weak::ptr_eq(&self.binding, &Arc::downgrade(&client.binding))
        {
            return Err(MihomoError::StaleBinding);
        }
        let matches = client.with_current_binding(|binding_generation| {
            binding_generation == self.binding_generation
                && session.committed_profile.read().generation == self.configuration_generation
        })?;
        if matches {
            Ok(())
        } else {
            Err(MihomoError::StaleBinding)
        }
    }

    pub(crate) fn publish_for_client<T>(
        &self,
        client: &MihomoClient,
        publish: impl FnOnce(u64) -> T,
    ) -> Option<T> {
        if !Weak::ptr_eq(&self.binding, &Arc::downgrade(&client.binding)) {
            return None;
        }
        self.publish_if_current(|| publish(self.binding_generation))
    }
    /// Returns the configuration generation captured by the read.
    #[must_use]
    pub fn configuration_generation(&self) -> u64 {
        self.configuration_generation
    }
    /// Returns whether this identity belongs to the specified session owner.
    #[must_use]
    pub fn belongs_to(&self, session: &CoreSession) -> bool {
        Weak::ptr_eq(
            &self.committed_profile,
            &Arc::downgrade(&session.committed_profile),
        ) && Weak::ptr_eq(&self.binding, &Arc::downgrade(&session.client.binding))
    }
    /// Tests current validity without performing I/O or waiting on a transition.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.publish_if_current(|| ()).is_some()
    }
    /// Atomically publishes ordinary read facts while the original runtime remains current.
    /// The callback must only mutate in-memory facts: it must not acquire session,
    /// client or repository locks, perform I/O, or begin another runtime operation.
    /// Returns `None` for a replaced/dead owner, changed configuration, or active transition.
    pub fn publish_if_current<T>(&self, publish: impl FnOnce() -> T) -> Option<T> {
        let shutdown = self.shutdown_requested.upgrade()?;
        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        let data_application = self.data_application.upgrade()?;
        let _data_application = data_application.try_lock().ok()?;
        let transition = self.transition.upgrade()?;
        let _transition = transition.try_lock().ok()?;
        let binding = self.binding.upgrade()?;
        let committed_profile = self.committed_profile.upgrade()?;
        binding
            .with_current_generation(self.binding_generation, || {
                let committed = committed_profile.read();
                if committed.generation != self.configuration_generation {
                    return None;
                }
                Some(publish())
            })
            .ok()
            .flatten()
    }
    /// Rejects stale read facts with the same typed binding error as the client.
    ///
    /// # Errors
    /// Returns a stale-binding error when the identity can no longer be published.
    pub fn verify_current(&self) -> Result<(), MihomoError> {
        if self.is_current() {
            Ok(())
        } else {
            Err(MihomoError::StaleBinding)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreKind, MihomoEndpoint};

    fn session() -> CoreSession {
        CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::default()).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn supplied_runtime_observation_rejects_a_different_actual_binding_with_equal_counters() {
        let session = session();
        let other = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        assert_eq!(
            session.client.binding.generation(),
            other.binding.generation()
        );
        assert!(session.begin_observation_for_runtime(&other).await.is_err());
    }

    #[tokio::test]
    async fn supplied_runtime_observation_keeps_the_same_owner_and_configuration_identity() {
        let session = session();
        let client = session.client().pin_current_runtime().unwrap();
        let observation = session
            .begin_observation_for_runtime(&client)
            .await
            .unwrap();
        assert!(observation.identity().belongs_to(&session));
        assert!(observation.client().verify_current_runtime().is_ok());
        session.invalidate_runtime_observations();
        assert!(!observation.identity().is_current());
    }

    #[tokio::test]
    async fn supplied_retired_runtime_does_not_recapture_the_replacement() {
        let session = session();
        let client = session.client().pin_current_runtime().unwrap();
        session
            .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", ""))
            .await
            .unwrap();
        assert!(
            session
                .begin_observation_for_runtime(&client)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn identity_cannot_publish_a_previous_configuration_generation() {
        let session = session();
        let observation = session.begin_observation().await.unwrap();
        let identity = observation.identity().clone();
        drop(observation);
        session.invalidate_runtime_observations();
        let mut published = false;
        assert!(identity.publish_if_current(|| published = true).is_none());
        assert!(!published);
    }

    #[tokio::test]
    async fn separate_reads_in_one_configuration_have_distinct_observation_identity() {
        let session = session();
        let first = session.begin_observation().await.unwrap();
        let second = session.begin_observation().await.unwrap();
        assert_eq!(
            first.identity().configuration_generation(),
            second.identity().configuration_generation()
        );
        assert!(
            first
                .identity()
                .is_same_observation(&first.identity().clone())
        );
        assert!(!first.identity().is_same_observation(second.identity()));
    }

    #[tokio::test]
    async fn separate_sessions_with_identical_counters_are_not_the_same_observation_owner() {
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        let first = CoreSession::open(CoreKind::Mihomo, client.clone()).unwrap();
        let second = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let observation = first.begin_observation().await.unwrap();
        assert!(observation.identity().belongs_to(&first));
        assert!(!observation.identity().belongs_to(&second));
        assert!(
            second
                .apply_observed_rule_disabled(observation.identity(), 0, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn observation_is_unpublishable_during_an_admitted_data_application() {
        let session = session();
        let identity = session
            .begin_observation()
            .await
            .unwrap()
            .identity()
            .clone();
        let admission = session.begin_data_application().await.unwrap();
        assert!(!identity.is_current());
        drop(admission);
        assert!(identity.is_current());
    }

    #[tokio::test]
    async fn cached_identity_does_not_keep_the_session_owner_alive() {
        let identity = {
            let session = session();
            session
                .begin_observation()
                .await
                .unwrap()
                .identity()
                .clone()
        };
        assert!(!identity.is_current());
    }

    #[tokio::test]
    async fn stale_rule_index_never_reaches_the_current_controller() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let identity = session
            .begin_observation()
            .await
            .unwrap()
            .identity()
            .clone();
        session.invalidate_runtime_observations();
        assert!(
            session
                .apply_observed_rule_disabled(&identity, 0, true)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }
}
