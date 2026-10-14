//! Proxy selection admission and owned acknowledgement in the observed configuration.

use super::{ControlledConfigError, CoreObservationIdentity, CoreSession, CoreSessionError};
use crate::{AcceptedProxySelection, ConnectionPolicy, ProxyOperations};

mod automatic_groups;
pub use automatic_groups::ProxyGroupActionReceipt;

impl CoreSession {
    /// Accepts a selection in the runtime/configuration that produced its target group.
    /// The session owns the write lease and transition through acknowledgement and cleanup;
    /// cancelling the foreground waiter cannot cancel admitted work. Readback is separate
    /// and the returned receipt holds no write authority.
    ///
    /// # Errors
    /// Returns stale/foreign observation, lifecycle, write-lease, controller or task errors.
    /// Transport failure after sending can leave the command outcome uncertain.
    pub async fn accept_observed_proxy_selection(
        &self,
        identity: &CoreObservationIdentity,
        group: &str,
        member: &str,
        connection_policy: ConnectionPolicy,
    ) -> Result<AcceptedProxySelection, CoreSessionError> {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        let group = group.to_owned();
        let member = member.to_owned();
        tokio::spawn(async move {
            // Move the entire admission: field-only capture would release its guards early.
            let owned_admission = admission;
            let accepted = ProxyOperations::new(owned_admission.client)
                .accept_selection(&group, &member, connection_policy)
                .await?;
            Ok(
                accepted.with_read_operations(ProxyOperations::from_observation(
                    &owned_admission.read_observation,
                )),
            )
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
