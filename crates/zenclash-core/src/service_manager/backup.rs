//! Backup authorization precedes data activation; consumption cannot authorize again.

use super::*;
use crate::core_session::service_tun::PreparedConfig;
use crate::{ControlledConfigStore, CoreRestoreSnapshot, PreparedBackupRestore};

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;

/// One frozen Local backup application, prepared before any live data is replaced.
/// Consumption requires the same session and the restore's existing write authority.
pub struct PreparedServiceBackupConfig {
    candidate: crate::backup::BackupRuntimeCandidate,
    previous: CoreRestoreSnapshot,
    config: PreparedConfig,
    request: ServiceTunRequest,
}

impl ServiceManager {
    /// Freezes a Local backup candidate and completes necessary native authorization.
    /// Returns no configuration for External, Service or experimental owners.
    /// No backup admission, data activation or kernel handover occurs here.
    ///
    /// # Errors
    /// Rejects invalid sources, mismatched roots, stale runtime and native authorization failure.
    pub async fn prepare_backup_restore(
        &self,
        prepared: PreparedBackupRestore,
    ) -> Result<(PreparedBackupRestore, Option<PreparedServiceBackupConfig>), ServiceManagerError>
    {
        if self.session.kind() != CoreKind::Mihomo
            || self.session.runtime_descriptor().backend() != CoreRuntimeBackend::Local
        {
            return Ok((prepared, None));
        }
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            manager
                .prepare_backup_restore_admitted(prepared, |manager, request| async move {
                    manager.authorize_service_request(&request).await?;
                    Ok(request)
                })
                .await
        })
        .await
    }

    async fn prepare_backup_restore_admitted<A, F>(
        &self,
        prepared: PreparedBackupRestore,
        authorize: A,
    ) -> Result<(PreparedBackupRestore, Option<PreparedServiceBackupConfig>), ServiceManagerError>
    where
        A: FnOnce(Self, ServiceTunRequest) -> F + Send,
        F: Future<Output = Result<ServiceTunRequest, ServiceManagerError>> + Send,
    {
        let mut request = self.request_enable_tun()?;
        let store = self
            .capture
            .controlled_store()
            .ok_or(ServiceManagerError::Unsupported)?;
        let (mut prepared, candidate) = tokio::task::spawn_blocking(move || {
            let candidate = prepared.runtime_candidate();
            (prepared, candidate)
        })
        .await
        .map_err(ServiceManagerError::Completion)?;
        let candidate = candidate.map_err(backup_failure)?;
        if candidate.data_root.join("controlled-config") != store.root() {
            return Err(ServiceManagerError::Unsupported);
        }
        self.check_intent(request.intent)?;
        let (config, previous) = {
            let _capture = self.session.capture_publication_gate().lock_owned().await;
            self.session
                .prepare_service_backup_config(
                    &store,
                    (request.intent.binding, request.intent.generation),
                    &candidate,
                )
                .await
                .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))?
        };
        if config.requires_service() {
            request = authorize(self.clone(), request.with_authorization()).await?;
        }
        self.check_intent(request.intent)?;
        request.allow_authorization = false;
        prepared.retain_runtime(
            (request.intent.binding, request.intent.generation),
            previous.clone(),
        );
        Ok((
            prepared,
            Some(PreparedServiceBackupConfig {
                candidate,
                previous,
                config,
                request,
            }),
        ))
    }

    /// Applies the prepared backup under the transaction's authorized controlled store.
    /// The caller keeps its owned restore completion and transaction alive until cleanup.
    /// This method never reopens native authorization after activation.
    ///
    /// # Errors
    /// Rejects changed data, stale runtime/snapshot, lost service readiness or runtime failure.
    pub async fn apply_backup_config(
        &self,
        store: &ControlledConfigStore,
        previous: &CoreRestoreSnapshot,
        prepared: PreparedServiceBackupConfig,
    ) -> Result<ServiceConfigOutcome, ServiceManagerError> {
        if !self.owns_profile_application(&self.session, store) {
            return Err(ServiceManagerError::Unsupported);
        }
        self.check_intent(prepared.request.intent)?;
        CoreSession::validate_backup_snapshot(&prepared.previous, previous)
            .map_err(|error| ServiceManagerError::Runtime(Box::new(error)))?;
        let lease = store
            .acquire_write_lease_for_paths(self.session.write_scopes())
            .await
            .map_err(|error| ServiceManagerError::Runtime(Box::new(error.into())))?;
        let store = store.with_write_lease(&lease);
        self.complete(ServiceOperation::EnableTun, move |manager| async move {
            let _lease = lease;
            manager.check_intent(prepared.request.intent)?;
            let worker_lease = store
                .acquire_write_lease()
                .await
                .map_err(|error| ServiceManagerError::Runtime(Box::new(error.into())))?;
            let candidate = prepared.candidate;
            tokio::task::spawn_blocking(move || candidate.validate_live(&worker_lease))
                .await
                .map_err(ServiceManagerError::Completion)?
                .map_err(backup_failure)?;
            manager.check_intent(prepared.request.intent)?;
            match prepared.config {
                PreparedConfig::Local(config) => manager
                    .session
                    .apply_prepared_local_config(&store, config)
                    .await
                    .map(ServiceConfigOutcome::Local)
                    .map_err(|error| ServiceManagerError::Runtime(Box::new(error))),
                PreparedConfig::Service(config) => {
                    let mut config = (*config).clone();
                    config.authorized_store = Some(store);
                    let mut request = prepared.request;
                    request.prepared_config = Some(Arc::new(config));
                    manager.capture.note_capture_intent();
                    manager
                        .enable_tun_admitted(request)
                        .await
                        .map(|outcome| ServiceConfigOutcome::Service(Box::new(outcome)))
                }
            }
        })
        .await
    }
}

fn backup_failure(error: crate::BackupError) -> ServiceManagerError {
    ServiceManagerError::Runtime(Box::new(CoreSessionError::Config(
        crate::ControlledConfigError::Transaction(error.to_string()),
    )))
}
