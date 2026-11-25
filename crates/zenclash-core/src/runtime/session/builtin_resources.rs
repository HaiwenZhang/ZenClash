//! Built-in resource updates owned by the existing session and their bound completion facts.

use super::{ControlledConfigError, CoreSession, CoreSessionError};
use crate::MihomoClient;

/// Successful built-in resource update and its original pinned controller client.
/// Temporary write authority and transition guards are not retained by this receipt.
pub struct BuiltinResourceUpdateReceipt {
    client: MihomoClient,
}

impl BuiltinResourceUpdateReceipt {
    /// Returns the original runtime for readback; requests reject its replacement.
    #[must_use]
    pub fn client(&self) -> &MihomoClient {
        &self.client
    }
}

enum BuiltinResource {
    GeoData,
    ExternalUi,
}

impl CoreSession {
    /// Updates GeoData through the same managed-directory and transition boundary.
    ///
    /// # Errors
    /// Returns lifecycle or controller failures.
    pub async fn update_geodata(&self) -> Result<(), CoreSessionError> {
        self.update_geodata_with_receipt().await.map(|_| ())
    }

    /// Updates the external UI files through the same managed-directory boundary.
    ///
    /// # Errors
    /// Returns lifecycle or controller failures.
    pub async fn update_external_ui(&self) -> Result<(), CoreSessionError> {
        self.update_external_ui_with_receipt().await.map(|_| ())
    }

    /// Updates GeoData and returns its original runtime for subsequent reads.
    ///
    /// # Errors
    /// Returns lifecycle, write-lease or controller failures.
    pub async fn update_geodata_with_receipt(
        &self,
    ) -> Result<BuiltinResourceUpdateReceipt, CoreSessionError> {
        self.update_builtin_resource(BuiltinResource::GeoData).await
    }

    /// Updates external UI files and returns the original runtime for subsequent reads.
    ///
    /// # Errors
    /// Returns lifecycle, write-lease or controller failures.
    pub async fn update_external_ui_with_receipt(
        &self,
    ) -> Result<BuiltinResourceUpdateReceipt, CoreSessionError> {
        self.update_builtin_resource(BuiltinResource::ExternalUi)
            .await
    }

    async fn update_builtin_resource(
        &self,
        resource: BuiltinResource,
    ) -> Result<BuiltinResourceUpdateReceipt, CoreSessionError> {
        let session = self.clone();
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let read_client = client.clone();
            let lease = session.acquire_runtime_write_lease(&client).await?;
            let client = client.with_write_lease(&lease)?;
            let _transition = session.transition.lock().await;
            client.ensure_binding_current()?;
            session.ensure_running_operations_allowed()?;
            match resource {
                BuiltinResource::GeoData => client.update_geodata().await?,
                BuiltinResource::ExternalUi => client.update_external_ui().await?,
            }
            Ok(BuiltinResourceUpdateReceipt {
                client: read_client,
            })
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests;
