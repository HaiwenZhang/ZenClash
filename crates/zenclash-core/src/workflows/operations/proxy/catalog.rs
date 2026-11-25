//! A whole proxy workspace fixed to one runtime and any supplied configuration identity.

use crate::{MihomoResult, ProxyCatalog};

use super::{ProxyOperations, ProxyVisibility};

/// Catalog and outbound-mode facts captured through the same runtime/configuration observation.
pub struct ProxyWorkspaceObservation {
    operations: ProxyOperations,
    catalog: ProxyCatalog,
    outbound_mode: String,
}

impl ProxyOperations {
    /// Reads catalog and outbound mode without mixing controller owners.
    ///
    /// # Errors
    /// Returns transport, controller, decoding or stale-binding errors.
    pub async fn observe_workspace(
        &self,
        visibility: ProxyVisibility,
    ) -> MihomoResult<ProxyWorkspaceObservation> {
        let operations = self.pin_current_runtime()?;
        let (catalog, config) = tokio::try_join!(
            operations.catalog(visibility),
            operations.client.runtime_config()
        )?;
        operations.verify_observation_current()?;
        Ok(ProxyWorkspaceObservation {
            operations,
            catalog,
            outbound_mode: config.mode,
        })
    }
}

impl ProxyWorkspaceObservation {
    /// Returns operations pinned to the runtime that produced this catalog.
    #[must_use]
    pub fn operations(&self) -> ProxyOperations {
        self.operations.clone()
    }

    /// Reads the catalog for bounded presentation preparation.
    #[must_use]
    pub fn catalog(&self) -> &ProxyCatalog {
        &self.catalog
    }
    /// Reads the outbound mode observed alongside the catalog.
    #[must_use]
    pub fn outbound_mode(&self) -> &str {
        &self.outbound_mode
    }
    /// Publishes both facts atomically against replacement of their original runtime.
    /// The callback must only change in-memory presentation facts and must not
    /// acquire client/session locks or perform I/O.
    ///
    /// # Errors
    /// Returns stale binding for runtime replacement or ineligible supplied configuration
    /// observation, without running the callback.
    pub fn publish_if_current<T>(
        self,
        publish: impl FnOnce(ProxyCatalog, String) -> T,
    ) -> MihomoResult<T> {
        self.operations
            .publish_if_current(|| publish(self.catalog, self.outbound_mode))
    }
}
