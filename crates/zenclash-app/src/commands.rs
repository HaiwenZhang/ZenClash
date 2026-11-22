//! Shared application commands and completion facts for pages, the tray, and automatic updates.

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

pub use backup_restore::{BackupRecoveryReceipt, BackupRestoration, BackupRestoreReceipt};
pub use override_application::{
    OverrideApplication, OverrideApplicationFailure, OverrideApplicationReceipt,
    OverrideFailureKind,
};
pub use profile_application::{DeletedProfileOutcome, ProfileFailure, ProfileReceipt};
use recovery_record::ApplicationRecoveryRecord;
use service_maintenance::service_failure_message;

/// Shared commands for profiles, effective configuration, maintenance, and recovery.
///
/// Clones share the same recovery record. Runtime ordering remains owned by
/// [`CoreSession`], and repository transactions remain owned by `ProfileApplication`.
#[derive(Clone)]
pub struct ApplicationCommands {
    session: CoreSession,
    overrides: Option<YamlOverrideStore>,
    recovery_record: ApplicationRecoveryRecord,
    service_manager: Option<ServiceManager>,
    maintenance_command: Arc<tokio::sync::Mutex<()>>,
}

impl ApplicationCommands {
    /// Creates an override-chain command bound to this shared recovery record.
    #[must_use]
    pub fn override_application(
        &self,
        store: YamlOverrideStore,
        controlled_config: ControlledConfigStore,
    ) -> OverrideApplication {
        OverrideApplication::new(self.clone(), store, controlled_config)
    }

    /// Creates a backup-restoration command bound to this shared recovery record.
    #[must_use]
    pub fn backup_restoration(&self, manager: zenclash_core::BackupManager) -> BackupRestoration {
        BackupRestoration::new(self.clone(), manager)
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

    /// Attaches the privileged service manager used for service-backed config,
    /// TUN handover, and maintenance commands.
    #[must_use]
    pub fn with_service_manager(mut self, manager: ServiceManager) -> Self {
        self.service_manager = Some(manager);
        self
    }

    /// Returns the underlying serialized runtime session.
    #[must_use]
    pub fn session(&self) -> &CoreSession {
        &self.session
    }

    /// Returns the typed controller client following the shared runtime binding.
    #[must_use]
    pub fn client(&self) -> &MihomoClient {
        self.session.client()
    }

    /// Returns the configured core kind for this session.
    #[must_use]
    pub fn kind(&self) -> CoreKind {
        self.session.kind()
    }

    /// Returns the latest unresolved profile runtime recovery context, if any.
    #[must_use]
    pub fn latest_recovery(&self) -> Option<ProfileRecovery> {
        self.recovery_record.latest_profile_recovery()
    }

    /// Returns the runtime generation awaiting native service commit confirmation.
    #[must_use]
    pub fn pending_finalization(&self) -> Option<u64> {
        self.recovery_record.pending_service_commit()
    }

    /// Returns the latest service TUN recovery or capture warning, if any.
    #[must_use]
    pub fn service_tun_warning(&self) -> Option<String> {
        self.recovery_record.service_recovery_warning()
    }

    /// Returns the currently committed profile snapshot from `CoreSession`.
    #[must_use]
    pub fn committed_profile(&self) -> CoreCommittedProfileSnapshot {
        self.session.committed_profile_snapshot()
    }

    /// Returns `true` when `version` matches the current runtime generation.
    #[must_use]
    pub fn is_current(&self, version: u64) -> bool {
        self.session.generation() == version
    }

    /// Records a service startup initialization receipt into the shared recovery record.
    pub fn record_service_initialization(&self, outcome: &CoreInitializationOutcome) {
        self.recovery_record.accept_initialization(
            outcome.saved(),
            outcome.commit_pending(),
            self.session.generation(),
        );
    }

    /// Returns the pending backup-restore generation awaiting retry, if any.
    #[must_use]
    pub fn pending_backup_restore(&self) -> Option<u64> {
        self.session.pending_backup_restore()
    }
}

#[cfg(test)]
mod tests;
