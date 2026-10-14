//! Native maintenance preparation, admitted execution and retained recovery resources.

use super::{ApplicationCommands, recovery_record::ServiceRuntimeFacts};
use std::{path::PathBuf, sync::Arc};
use zenclash_core::{
    ControlledConfigStore, ServiceHealthKind, ServiceManager, ServiceManagerError,
    ServiceManagerSnapshot,
};

impl ApplicationCommands {
    pub(crate) fn service_state(&self) -> Option<ServiceManagerSnapshot> {
        self.service_manager.as_ref().map(ServiceManager::snapshot)
    }

    pub(crate) fn service_updates(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<ServiceManagerSnapshot>> {
        self.service_manager.as_ref().map(ServiceManager::subscribe)
    }
    /// Captures a maintenance intent for the current runtime without performing I/O.
    ///
    /// # Errors
    /// Returns an error when maintenance is unsupported or the runtime is unresolved.
    pub fn request_service_maintenance(
        &self,
        operation: zenclash_core::ServiceOperation,
        fallback: Option<zenclash_core::CoreLaunchConfig>,
    ) -> Result<zenclash_core::ServiceMaintenanceRequest, String> {
        self.service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?
            .request_maintenance(operation, fallback)
            .map_err(service_failure_message)
    }

    /// Resolves ordinary identity for direct Service maintenance before confirmation.
    /// Manager owns background discovery and its prepared progress.
    ///
    /// # Errors
    /// Returns unsupported/stale intent, overlapping command or executable discovery errors.
    pub async fn discover_service_maintenance(
        &self,
        operation: zenclash_core::ServiceOperation,
        project_root: PathBuf,
        preferred_binary: Option<PathBuf>,
    ) -> Result<zenclash_core::ServiceMaintenanceRequest, String> {
        self.service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?
            .discover_maintenance_request(operation, project_root, preferred_binary)
            .await
            .map_err(service_failure_message)
    }

    /// Prepares maintenance and retains its receipt independently of any page waiter.
    /// No administrator operation is submitted by this method.
    ///
    /// # Errors
    /// Returns runtime preparation errors; an unready receipt remains available to callers.
    pub async fn prepare_service_maintenance(
        &self,
        request: zenclash_core::ServiceMaintenanceRequest,
        store: &ControlledConfigStore,
    ) -> Result<Arc<zenclash_core::ServiceMaintenancePreparation>, String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let store = store.clone();
        let commands = self.clone();
        let command = self
            .maintenance_command
            .clone()
            .try_lock_owned()
            .map_err(|_| service_failure_message(ServiceManagerError::Busy))?;
        tokio::spawn(async move {
            let _command = command;
            let preparation = Arc::new(
                manager
                    .prepare_maintenance(request, &store)
                    .await
                    .map_err(service_failure_message)?,
            );
            commands
                .recovery_record
                .accept_maintenance_preparation(preparation.clone(), &commands.session);
            Ok(preparation)
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    /// Reads the latest maintenance preparation without I/O.
    pub fn service_maintenance_preparation(
        &self,
    ) -> Option<Arc<zenclash_core::ServiceMaintenancePreparation>> {
        self.recovery_record.maintenance_preparation()
    }

    /// Reads the last Service-to-Local resources, preserved across later Local commands.
    /// The core receipt still checks identity and generation before any resource reuse.
    pub fn service_local_recovery(
        &self,
    ) -> Option<Arc<zenclash_core::ServiceMaintenancePreparation>> {
        self.recovery_record.local_recovery_preparation()
    }

    /// Submits the latest preparation with the caller's explicit authorization choice.
    /// Keeps recovery resources after success, cancellation, or failure.
    ///
    /// # Errors
    /// Rejects replaced/foreign receipts, missing consent, stale runtime or native failure.
    pub async fn maintain_prepared_service(
        &self,
        preparation: Arc<zenclash_core::ServiceMaintenancePreparation>,
        allow_authorization: bool,
    ) -> Result<(), String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let command = self
            .maintenance_command
            .clone()
            .try_lock_owned()
            .map_err(|_| service_failure_message(ServiceManagerError::Busy))?;
        if !self
            .recovery_record
            .is_latest_maintenance_preparation(&preparation)
        {
            return Err(service_failure_message(ServiceManagerError::Stale));
        }
        let preparation = if allow_authorization {
            preparation.as_ref().clone().with_authorization()
        } else {
            preparation.as_ref().clone()
        };
        let commands = self.clone();
        tokio::spawn(async move {
            let _command = command;
            let maintenance = manager.maintain_prepared(&preparation).await;
            let recoverable = allow_authorization
                && preparation.operation() == zenclash_core::ServiceOperation::Repair
                && preparation.recovery().is_some()
                && maintenance.as_ref().err().is_none_or(|error| {
                    !error.is_native_outcome_unconfirmed()
                        && !matches!(
                            error,
                            ServiceManagerError::Stale
                                | ServiceManagerError::Closed
                                | ServiceManagerError::Busy
                                | ServiceManagerError::ConsentRequired
                        )
                });
            let recovery = if recoverable {
                match manager.restore_prepared_capture(&preparation).await {
                    Ok(outcome) => {
                        if let Some(tun) = outcome.tun()
                            && let Some(core) = tun.core()
                        {
                            commands.recovery_record.accept_service_runtime(
                                ServiceRuntimeFacts::new(
                                    core,
                                    tun.commit_pending(),
                                    outcome.warning().or(tun.recovery_warning()),
                                ),
                                commands.session.generation(),
                            );
                        }
                        outcome.warning().map(str::to_owned)
                    }
                    Err(error) => Some(service_failure_message(error)),
                }
            } else {
                None
            };
            match (maintenance, recovery) {
                (Ok(()), None) => Ok(()),
                (Ok(()), Some(warning)) => Err(warning),
                (Err(error), None) => Err(service_failure_message(error)),
                (Err(error), Some(warning)) => {
                    Err(format!("{}; {warning}", service_failure_message(error)))
                }
            }
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }
}

pub(super) fn service_failure_message(error: ServiceManagerError) -> String {
    if error.is_authorization_cancelled() {
        return zenclash_i18n::text("core_page.service.cancelled");
    }
    if error.is_native_outcome_unconfirmed() {
        return zenclash_i18n::text("core_page.service.pending");
    }
    let key = match error {
        ServiceManagerError::Busy => "core_page.service.busy",
        ServiceManagerError::Stale | ServiceManagerError::Closed => "core_page.service.stale",
        ServiceManagerError::Unsupported => "core_page.service.unsupported",
        ServiceManagerError::ConsentRequired => "core_page.service.install_description",
        ServiceManagerError::Sources(_) => "core_page.service.bundle_missing",
        ServiceManagerError::Health(kind) => match kind {
            ServiceHealthKind::NotInstalled => "core_page.service.missing",
            ServiceHealthKind::Ready | ServiceHealthKind::Unknown => "core_page.service.unknown",
            ServiceHealthKind::Unavailable(_) => "core_page.service.unavailable",
            ServiceHealthKind::VersionMismatch => "core_page.service.incompatible",
        },
        ServiceManagerError::Runtime(error) => return error.to_string(),
        ServiceManagerError::Maintenance(_) => "core_page.service.failed",
        _ => "core_page.service.unknown",
    };
    zenclash_i18n::text(key)
}
