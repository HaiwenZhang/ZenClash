//! Owned automatic-selection restoration and group-delay actions on their observed source.

use super::{ControlledConfigError, CoreObservationIdentity, CoreSession, CoreSessionError};
use crate::{ProxyGroupMeasurementOutcome, ProxyOperations, ProxySelectionOutcome};

/// Completed proxy-group action and its unleased original read/publication context.
/// Readback failures remain outcome warnings after the command has been accepted.
pub struct ProxyGroupActionReceipt<Outcome> {
    operations: ProxyOperations,
    outcome: Outcome,
}

impl<Outcome> ProxyGroupActionReceipt<Outcome> {
    /// Returns the original runtime/configuration context, without write authority.
    #[must_use]
    pub fn operations(&self) -> &ProxyOperations {
        &self.operations
    }

    /// Returns the command's outcome, including any readback warnings.
    #[must_use]
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// Moves the original context and outcome into a presentation consumer.
    #[must_use]
    pub fn into_parts(self) -> (ProxyOperations, Outcome) {
        (self.operations, self.outcome)
    }
}

impl CoreSession {
    /// Measures a group without requesting a catalog readback, retaining the original source.
    /// The group's delay side effects stay inside the existing owned mutation admission.
    ///
    /// # Errors
    /// Returns source, lifecycle, write-lease, delay request or task errors.
    pub async fn measure_observed_proxy_group_delays(
        &self,
        identity: &CoreObservationIdentity,
        group: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ProxyGroupActionReceipt<std::collections::BTreeMap<String, u32>>, CoreSessionError>
    {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        let group = group.to_owned();
        let test_url = test_url.map(str::to_owned);
        tokio::spawn(async move {
            let owned_admission = admission;
            let outcome = ProxyOperations::new(owned_admission.client)
                .measure_group(&group, test_url.as_deref(), timeout_ms)
                .await?;
            Ok(ProxyGroupActionReceipt {
                operations: ProxyOperations::from_observation(&owned_admission.read_observation),
                outcome,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    /// Restores automatic group selection and reads back facts in its original configuration.
    /// The owned action holds the existing write/transition admission through the request
    /// and best-effort catalog readback, even if its foreground waiter is cancelled.
    ///
    /// # Errors
    /// Returns source, lifecycle, write-lease, controller command or task errors.
    /// Readback failure after restoration succeeds is returned as an outcome warning.
    pub async fn restore_observed_proxy_group(
        &self,
        identity: &CoreObservationIdentity,
        group: &str,
    ) -> Result<ProxyGroupActionReceipt<ProxySelectionOutcome>, CoreSessionError> {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        let group = group.to_owned();
        tokio::spawn(async move {
            // Move the entire admission: field-only capture would release its guards early.
            let owned_admission = admission;
            let outcome = ProxyOperations::new(owned_admission.client)
                .restore_auto(&group)
                .await?;
            Ok(ProxyGroupActionReceipt {
                operations: ProxyOperations::from_observation(&owned_admission.read_observation),
                outcome,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }

    /// Measures a group and reads back its selection in the configuration that produced it.
    /// Mihomo's group-delay selection side effect is preserved. The owned action retains
    /// write/transition admission through the delay response and best-effort readback;
    /// its completion receipt holds no write authority.
    ///
    /// # Errors
    /// Returns source, lifecycle, write-lease, delay request or task errors.
    /// Readback failure after measurement succeeds remains an outcome warning.
    pub async fn measure_observed_proxy_group(
        &self,
        identity: &CoreObservationIdentity,
        group: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ProxyGroupActionReceipt<ProxyGroupMeasurementOutcome>, CoreSessionError> {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        let group = group.to_owned();
        let test_url = test_url.map(str::to_owned);
        tokio::spawn(async move {
            // Move the entire admission: field-only capture would release its guards early.
            let owned_admission = admission;
            let outcome = ProxyOperations::new(owned_admission.client)
                .measure_group_and_restore_auto(&group, test_url.as_deref(), timeout_ms)
                .await?;
            Ok(ProxyGroupActionReceipt {
                operations: ProxyOperations::from_observation(&owned_admission.read_observation),
                outcome,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests;
