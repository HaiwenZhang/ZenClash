//! Resource actions admitted in their original configuration and retained read source.

use super::{
    ControlledConfigError, CoreObservation, CoreObservationIdentity, CoreSession, CoreSessionError,
};
use crate::{ProviderKind, ProviderOperations, ProviderUpdateTarget};

/// Resource intent submitted against the configuration that produced its target.
#[non_exhaustive]
pub enum CoreResourceAction {
    /// Update GeoData files through Mihomo.
    GeoData,
    /// Update the external UI files through Mihomo.
    ExternalUi,
    /// Update one provider by controller key.
    UpdateProvider {
        /// Provider collection kind.
        kind: ProviderKind,
        /// Controller map key.
        key: String,
    },
    /// Health-check one proxy provider.
    HealthcheckProvider {
        /// Controller map key.
        key: String,
    },
    /// Update a fixed target set, retaining ordinary target failures.
    UpdateProviders(Vec<ProviderUpdateTarget>),
}

/// Completed resource action and its original unleased observation for presentation reads.
pub struct ResourceActionReceipt {
    observation: CoreObservation,
    failures: Vec<String>,
}

impl ResourceActionReceipt {
    /// Returns the original runtime/configuration/query source, without mutation authority.
    #[must_use]
    pub fn observation(&self) -> &CoreObservation {
        &self.observation
    }
    /// Returns failures retained by a provider update batch.
    #[must_use]
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
    /// Moves the original observation and batch failures into the consumer.
    #[must_use]
    pub fn into_parts(self) -> (CoreObservation, Vec<String>) {
        (self.observation, self.failures)
    }
}

impl CoreSession {
    /// Performs a resource action under existing original-runtime write/transition admission.
    /// The owned task keeps all admission through controller requests, readback and history
    /// adoption despite waiter cancellation. Subsequent reads use the returned original source.
    ///
    /// # Errors
    /// Returns foreign/stale source, lifecycle, write-lease, action/readback or task errors.
    /// Individual batch target errors are retained by the receipt.
    pub async fn apply_observed_resource_action(
        &self,
        identity: &CoreObservationIdentity,
        providers: &ProviderOperations,
        action: CoreResourceAction,
    ) -> Result<ResourceActionReceipt, CoreSessionError> {
        let admission = self.admit_observed_runtime_mutation(identity).await?;
        let providers = providers
            .with_admitted_client(admission.client.clone())
            .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
        tokio::spawn(async move {
            // Moving the whole admission keeps its guards out of precise field capture.
            let owned_admission = admission;
            let failures = match action {
                CoreResourceAction::GeoData => {
                    owned_admission.client.update_geodata().await?;
                    Vec::new()
                }
                CoreResourceAction::ExternalUi => {
                    owned_admission.client.update_external_ui().await?;
                    Vec::new()
                }
                CoreResourceAction::UpdateProvider { kind, key } => {
                    providers
                        .update_with_receipt(kind, &key)
                        .await
                        .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
                    Vec::new()
                }
                CoreResourceAction::HealthcheckProvider { key } => {
                    providers
                        .healthcheck_proxy_with_receipt(&key)
                        .await
                        .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
                    Vec::new()
                }
                CoreResourceAction::UpdateProviders(targets) => providers
                    .update_many(targets)
                    .await
                    .map_err(|error| ControlledConfigError::Task(error.to_string()))?
                    .failures()
                    .to_vec(),
            };
            Ok(ResourceActionReceipt {
                observation: owned_admission.read_observation,
                failures,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests;
