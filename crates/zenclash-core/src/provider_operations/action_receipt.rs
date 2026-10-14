//! Completed provider facts and the runtime used for subsequent presentation reads.

use crate::{MihomoClient, ProviderCatalog};

/// Successful provider action and catalog readback from its original runtime.
pub struct ProviderActionReceipt {
    client: MihomoClient,
    catalog: ProviderCatalog,
}

impl ProviderActionReceipt {
    pub(super) fn new(client: MihomoClient, catalog: ProviderCatalog) -> Self {
        Self { client, catalog }
    }

    /// Returns the original pinned client, which rejects a replacement runtime.
    #[must_use]
    pub fn client(&self) -> &MihomoClient {
        &self.client
    }

    /// Returns the successful readback without claiming it is still current.
    #[must_use]
    pub fn catalog(&self) -> &ProviderCatalog {
        &self.catalog
    }

    pub(super) fn into_catalog(self) -> ProviderCatalog {
        self.catalog
    }
}
