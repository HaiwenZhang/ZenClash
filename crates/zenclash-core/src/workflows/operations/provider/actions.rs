//! Provider updates and health checks bound to one original controller runtime.
use super::{
    ProviderAction, ProviderActionReceipt, ProviderKind, ProviderOperationError,
    ProviderOperationResult, ProviderOperations, ProviderUpdateBatchReceipt, ProviderUpdateTarget,
};
use crate::ProviderCatalog;

impl ProviderOperations {
    /// Downloads a provider and returns its validated catalog readback.
    ///
    /// # Errors
    /// Returns controller, readback, missing-provider or stale-runtime errors.
    pub async fn update(
        &self,
        kind: ProviderKind,
        name: &str,
    ) -> ProviderOperationResult<ProviderCatalog> {
        self.update_with_receipt(kind, name)
            .await
            .map(ProviderActionReceipt::into_catalog)
    }

    /// Downloads a fresh provider document and verifies it through catalog readback.
    ///
    /// A failed update records its own failure while preserving the previous
    /// provider value and count.
    ///
    /// # Errors
    ///
    /// Returns controller or readback errors.
    pub async fn update_with_receipt(
        &self,
        kind: ProviderKind,
        name: &str,
    ) -> ProviderOperationResult<ProviderActionReceipt> {
        let client = self
            .client
            .pin_binding()
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
        let result = async {
            match kind {
                ProviderKind::Proxy => client.update_proxy_provider(name).await,
                ProviderKind::Rule => client.update_rule_provider(name).await,
            }
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
            Self::catalog_on(&client, kind)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))
        }
        .await;
        let catalog = client
            .with_current_binding(|generation| {
                self.finish_action(generation, kind, name, ProviderAction::Update, result)
            })
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))??;
        Ok(ProviderActionReceipt::new(client, catalog))
    }

    /// Updates a whole target set without migrating later operations to a new runtime.
    /// A runtime replacement stops the remaining batch; ordinary target failures
    /// are retained while other targets continue on the original runtime.
    ///
    /// # Errors
    /// Returns an initial stale-binding error before any target is attempted.
    pub async fn update_many(
        &self,
        targets: Vec<ProviderUpdateTarget>,
    ) -> ProviderOperationResult<ProviderUpdateBatchReceipt> {
        let client = self
            .client
            .pin_binding()
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
        let operations = Self {
            client: client.clone(),
            observations: self.observations.clone(),
        };
        let mut failures = Vec::new();
        for target in targets {
            if let Err(error) = client.ensure_binding_current() {
                failures.push(error.to_string());
                break;
            }
            if let Err(error) = operations.update(target.kind, &target.controller_key).await {
                failures.push(format!("{}: {error}", target.controller_key));
            }
        }
        Ok(ProviderUpdateBatchReceipt { client, failures })
    }

    /// Runs a proxy-provider health check and returns its validated catalog readback.
    ///
    /// # Errors
    /// Returns controller, readback, missing-provider or stale-runtime errors.
    pub async fn healthcheck_proxy(&self, name: &str) -> ProviderOperationResult<ProviderCatalog> {
        self.healthcheck_proxy_with_receipt(name)
            .await
            .map(ProviderActionReceipt::into_catalog)
    }

    /// Runs a proxy-provider health check without downloading a new document.
    ///
    /// Update and health-check histories remain independent.
    ///
    /// # Errors
    ///
    /// Returns controller or readback errors.
    pub async fn healthcheck_proxy_with_receipt(
        &self,
        name: &str,
    ) -> ProviderOperationResult<ProviderActionReceipt> {
        let client = self
            .client
            .pin_binding()
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
        let result = async {
            client
                .healthcheck_proxy_provider(name)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
            Self::catalog_on(&client, ProviderKind::Proxy)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))
        }
        .await;
        let catalog = client
            .with_current_binding(|generation| {
                self.finish_action(
                    generation,
                    ProviderKind::Proxy,
                    name,
                    ProviderAction::Healthcheck,
                    result,
                )
            })
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))??;
        Ok(ProviderActionReceipt::new(client, catalog))
    }
}
