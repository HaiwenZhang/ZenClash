//! Intent-oriented proxy selection and delay measurement.

use crate::{
    CoreObservation, CoreObservationIdentity, MihomoClient, MihomoError, MihomoResult, ProxyCatalog,
};

mod catalog;
mod measurement;
pub use catalog::ProxyWorkspaceObservation;
#[cfg(test)]
mod runtime_tests;
mod selection;
#[cfg(test)]
mod tests;

pub use measurement::{ProxyDelayTarget, ProxyGroupMeasurementOutcome};
pub use selection::{
    AcceptedProxySelection, ConnectionPolicy, ObservedProxySelection, ProxySelectionOutcome,
    ProxySelectionReadback, ProxySelectionReceipt,
};

/// Hidden-group visibility requested by a proxy catalog consumer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyVisibility {
    /// Return only groups intended for normal user-facing surfaces.
    #[default]
    VisibleOnly,
    /// Include profile-internal groups marked hidden by Mihomo.
    IncludeHidden,
}

/// Deep interface for proxy selection and provider-aware latency checks.
#[derive(Clone)]
pub struct ProxyOperations {
    client: MihomoClient,
    observation_identity: Option<CoreObservationIdentity>,
}

impl ProxyOperations {
    /// Creates proxy operations over one Mihomo controller client.
    #[must_use]
    pub fn new(client: MihomoClient) -> Self {
        Self {
            client,
            observation_identity: None,
        }
    }

    /// Creates operations retaining the runtime and configuration that produced a read.
    /// The identity controls read/publication eligibility; it grants no mutation authority.
    #[must_use]
    pub fn from_observation(observation: &CoreObservation) -> Self {
        Self::new(observation.client().clone())
            .with_observation_identity(observation.identity().clone())
    }

    /// Returns the configuration observation retained by this catalog's operations.
    /// Its identity is an eligibility constraint, not mutation authority.
    #[must_use]
    pub fn observation_identity(&self) -> Option<&CoreObservationIdentity> {
        self.observation_identity.as_ref()
    }

    pub(crate) fn with_observation_identity(mut self, identity: CoreObservationIdentity) -> Self {
        self.observation_identity = Some(identity);
        self
    }

    /// Captures one runtime binding for a complete operation or measurement batch.
    /// Clones keep this pin and any configuration observation supplied at construction.
    ///
    /// # Errors
    /// Returns a stale-binding error for a retired runtime or an ineligible observation
    /// (changed configuration, active transition/data application, shutdown or dead owner).
    pub fn pin_current_runtime(&self) -> MihomoResult<Self> {
        let operations = Self {
            client: self.client.pin_binding()?,
            observation_identity: self.observation_identity.clone(),
        };
        operations.verify_observation_current()?;
        Ok(operations)
    }

    /// Publishes in-memory facts while this operation's runtime binding and any
    /// supplied configuration observation are current.
    /// Callers retaining a batch must pin it before starting requests. The callback
    /// must not acquire client/session locks, perform I/O or start runtime mutations.
    ///
    /// # Errors
    /// Returns a stale-binding error without invoking the callback for a retired runtime
    /// or an ineligible observation (including configuration changes and active transitions).
    pub fn publish_if_current<T>(&self, publish: impl FnOnce() -> T) -> MihomoResult<T> {
        self.client.ensure_binding_current()?;
        match &self.observation_identity {
            Some(identity) => identity
                .publish_for_client(&self.client, |_| publish())
                .ok_or(MihomoError::StaleBinding),
            None => self.client.with_current_binding(|_| publish()),
        }
    }

    fn verify_observation_current(&self) -> MihomoResult<()> {
        self.publish_if_current(|| ())
    }

    /// Fetches the current proxy catalog with an explicit hidden-group policy.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status, response-decoding or stale-observation errors.
    /// A supplied observation is checked before and after the read; it can become
    /// ineligible during configuration changes, active transitions or owner shutdown.
    pub async fn catalog(&self, visibility: ProxyVisibility) -> MihomoResult<ProxyCatalog> {
        self.verify_observation_current()?;
        let mut catalog = self.client.proxy_catalog().await?;
        self.verify_observation_current()?;
        if visibility == ProxyVisibility::VisibleOnly {
            catalog.hide_hidden_groups();
        }
        Ok(catalog)
    }
}
