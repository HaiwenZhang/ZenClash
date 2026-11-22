//! Original CoreSession observation retained across read worker failure.

use std::sync::OnceLock;
use zenclash_core::{CoreObservation, CoreObservationIdentity, CoreSession, MihomoError};

/// Written before I/O, independently of the task result. It grants no mutation authority.
#[derive(Default)]
pub(crate) struct RuntimeReadSource(OnceLock<CoreObservationIdentity>);

impl RuntimeReadSource {
    pub(crate) async fn capture(&self, session: &CoreSession) -> Result<CoreObservation, String> {
        let observation = session
            .begin_observation()
            .await
            .map_err(|error| error.to_string())?;
        self.0
            .set(observation.identity().clone())
            .map_err(|_| MihomoError::StaleBinding.to_string())?;
        Ok(observation)
    }

    pub(crate) fn identity(&self) -> Option<&CoreObservationIdentity> {
        self.0.get()
    }
}
