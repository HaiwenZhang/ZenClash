//! Connection closure admitted in the configuration that supplied the displayed connections.

use super::{
    ControlledConfigError, CoreObservation, CoreObservationIdentity, CoreSession, CoreSessionError,
};

impl CoreSession {
    /// Closes one connection, or all connections when `id` is `None`, in the observed runtime.
    /// Returns the original source for readback without retaining mutation authority.
    /// Admitted completion survives cancellation of its foreground waiter.
    ///
    /// # Errors
    /// Returns stale source, shutdown, write-admission, controller or task errors.
    pub async fn close_observed_connections(
        &self,
        identity: &CoreObservationIdentity,
        id: Option<String>,
    ) -> Result<CoreObservation, CoreSessionError> {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        tokio::spawn(async move {
            let owned_admission = admission;
            match id {
                Some(id) => owned_admission.client.close_connection(&id).await?,
                None => owned_admission.client.close_all_connections().await?,
            }
            Ok(owned_admission.read_observation)
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests;
