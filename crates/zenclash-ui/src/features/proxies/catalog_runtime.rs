//! Original-runtime eligibility for catalog-derived operations and delay publication.

use super::{Context, DelayTaskToken, ProxiesPage, ProxyOperations};

/// Written once before controller I/O so even a panicked read retains its original source.
#[derive(Default)]
pub(super) struct ProxyCatalogReadSource(std::sync::OnceLock<ProxyOperations>);

impl ProxyCatalogReadSource {
    pub(super) async fn capture(
        &self,
        session: Option<zenclash_core::CoreSession>,
        client: zenclash_core::MihomoClient,
    ) -> Result<ProxyOperations, String> {
        let operations = match session {
            Some(session) => {
                let observation = session
                    .begin_observation_for_runtime(&client)
                    .await
                    .map_err(|error| error.to_string())?;
                ProxyOperations::from_observation(&observation)
            }
            None => ProxyOperations::new(client),
        }
        .pin_current_runtime()
        .map_err(|error| error.to_string())?;
        self.0
            .set(operations.clone())
            .map_err(|_| zenclash_core::MihomoError::StaleBinding.to_string())?;
        Ok(operations)
    }

    fn publish_failure(&self, publish: impl FnOnce()) -> bool {
        match self.0.get() {
            Some(source) => source.publish_if_current(publish).is_ok(),
            // Capture failed before controller I/O; this is a query-local preflight failure.
            None => {
                publish();
                true
            }
        }
    }
}

impl ProxiesPage {
    pub(super) fn publish_catalog_refresh_error(
        &mut self,
        token: super::CatalogTaskToken,
        source: &ProxyCatalogReadSource,
        error: String,
    ) -> bool {
        if let Some(session) = &self.core_session
            && let Some(operations) = source.0.get()
            && !operations
                .observation_identity()
                .is_some_and(|identity| identity.belongs_to(session))
        {
            return false;
        }
        if !token.is_current(self.catalog_generation) {
            return false;
        }
        let published = source.publish_failure(|| self.error = Some(error));
        if !published && self.error.is_none() {
            // A present view still needs an eligibility signal while its first read is stale.
            self.error = Some(zenclash_core::MihomoError::StaleBinding.to_string());
        }
        published
    }
    pub(super) fn capture_catalog_operations(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<ProxyOperations> {
        let operations = match &self.catalog_operations {
            Some(operations) => operations.clone().pin_current_runtime(),
            None if self.core_session.is_none() => {
                ProxyOperations::new(self.client.clone()).pin_current_runtime()
            }
            None => Err(zenclash_core::MihomoError::StaleBinding),
        };
        match operations {
            Ok(operations) => Some(operations),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                None
            }
        }
    }

    pub(super) fn capture_catalog_mutation_source(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<ProxyOperations> {
        if self.core_session.is_none() {
            return self.capture_catalog_operations(cx);
        }
        // Publication requires idle gates; mutation admission may queue behind another group.
        // The session checks this retained source after acquiring its original write/transition gates.
        match self.catalog_operations.clone() {
            Some(operations) => Some(operations),
            None => {
                self.error = Some(zenclash_core::MihomoError::StaleBinding.to_string());
                cx.notify();
                None
            }
        }
    }

    pub(super) fn publish_delay_result(
        &mut self,
        token: DelayTaskToken,
        operations: &ProxyOperations,
        publish: impl FnOnce(&mut Self),
    ) -> bool {
        token.is_current(self.delay_generation)
            && operations.publish_if_current(|| publish(self)).is_ok()
    }
}

#[cfg(test)]
mod tests;
