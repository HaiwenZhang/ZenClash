//! Shared application commands and completion facts for pages, the tray and automatic updates.

use std::sync::Arc;

use zenclash_core::{
    ControlledConfigStore, CoreCommittedProfileSnapshot, CoreInitializationOutcome, CoreKind,
    CoreSession, MihomoClient, ProfileRecovery, ServiceManager, YamlOverrideStore,
};

mod backup_restore;
mod configuration_application;
mod override_application;
mod profile_application;
mod recovery_record;
mod service_maintenance;
mod service_tun;

pub(crate) use backup_restore::BackupRestoreReceipt;
pub(crate) use override_application::{
    OverrideApplicationFailure, OverrideApplicationReceipt, OverrideFailureKind,
};
pub(crate) use profile_application::ProfileReceipt;
use recovery_record::ApplicationRecoveryRecord;
use service_maintenance::service_failure_message;

/// Shared commands for profiles, effective configuration, maintenance and recovery.
///
/// Clones share the same recovery record. Runtime ordering remains owned by
/// `CoreSession`, and repository transactions remain owned by `ProfileApplication`.
#[derive(Clone)]
pub struct ApplicationCommands {
    session: CoreSession,
    overrides: Option<YamlOverrideStore>,
    recovery_record: ApplicationRecoveryRecord,
    service_manager: Option<ServiceManager>,
    maintenance_command: Arc<tokio::sync::Mutex<()>>,
}

impl ApplicationCommands {
    pub(crate) fn override_application(
        &self,
        store: YamlOverrideStore,
        controlled_config: ControlledConfigStore,
    ) -> override_application::OverrideApplication {
        override_application::OverrideApplication::new(self.clone(), store, controlled_config)
    }

    pub(crate) fn backup_restoration(
        &self,
        manager: zenclash_core::BackupManager,
    ) -> backup_restore::BackupRestoration {
        backup_restore::BackupRestoration::new(self.clone(), manager)
    }

    /// Composes existing core services without creating another runtime owner.
    #[must_use]
    pub fn new(session: CoreSession, overrides: Option<YamlOverrideStore>) -> Self {
        Self {
            session,
            overrides,
            recovery_record: ApplicationRecoveryRecord::default(),
            service_manager: None,
            maintenance_command: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn with_service_manager(mut self, manager: ServiceManager) -> Self {
        self.service_manager = Some(manager);
        self
    }

    pub(crate) fn session(&self) -> &CoreSession {
        &self.session
    }

    pub(crate) fn client(&self) -> &MihomoClient {
        self.session.client()
    }

    pub(crate) fn kind(&self) -> CoreKind {
        self.session.kind()
    }

    pub(crate) fn latest_recovery(&self) -> Option<ProfileRecovery> {
        self.recovery_record.latest_profile_recovery()
    }

    pub(crate) fn pending_finalization(&self) -> Option<u64> {
        self.recovery_record.pending_service_commit()
    }

    pub(crate) fn service_tun_warning(&self) -> Option<String> {
        self.recovery_record.service_recovery_warning()
    }

    pub(crate) fn committed_profile(&self) -> CoreCommittedProfileSnapshot {
        self.session.committed_profile_snapshot()
    }

    pub(crate) fn is_current(&self, version: u64) -> bool {
        self.session.generation() == version
    }

    pub(crate) fn record_service_initialization(&self, outcome: &CoreInitializationOutcome) {
        self.recovery_record.accept_initialization(
            outcome.saved(),
            outcome.commit_pending(),
            self.session.generation(),
        );
    }

    pub(crate) fn pending_backup_restore(&self) -> Option<u64> {
        self.session.pending_backup_restore()
    }
}

#[cfg(test)]
mod tests;
