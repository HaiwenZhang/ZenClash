//! Existing runtime mutation admission constrained by an actual captured observation.

use super::{
    CommittedConfig, CoreObservation, CoreObservationIdentity, CoreSession, CoreSessionError,
    DataWriteLease,
};
use crate::{MihomoClient, MihomoError};

pub(super) struct ObservedRuntimeMutationAdmission {
    pub(super) client: MihomoClient,
    pub(super) read_observation: CoreObservation,
    _write_lease: DataWriteLease,
    _transition: tokio::sync::OwnedMutexGuard<CommittedConfig>,
    _session_owner: CoreSession,
}

impl CoreSession {
    pub(super) async fn admit_observed_runtime_mutation(
        &self,
        identity: &CoreObservationIdentity,
    ) -> Result<ObservedRuntimeMutationAdmission, CoreSessionError> {
        if !identity.belongs_to(self) {
            return Err(MihomoError::StaleBinding.into());
        }
        let client = self.client.pin_binding()?;
        let lease = self.acquire_runtime_write_lease(&client).await?;
        let transition = self.transition.clone().lock_owned().await;
        self.ensure_running_operations_allowed()?;
        identity.verify_admitted_runtime(self, &client)?;
        let read_observation = identity.retain_admitted_read(client.clone());
        Ok(ObservedRuntimeMutationAdmission {
            client: client.with_write_lease(&lease)?,
            read_observation,
            _write_lease: lease,
            _transition: transition,
            _session_owner: self.clone(),
        })
    }
}
