//! Complete backup restoration and recovery, independent of page presentation.

use zenclash_core::BackupManager;

use super::ApplicationCommands;

mod recovery;
mod restore;
mod state;
#[cfg(test)]
mod tests;

pub(crate) use state::{BackupRecoveryReceipt, BackupRestoreReceipt};

/// Binds archive/disk ownership to the original shared command recovery record.
#[derive(Clone)]
pub(crate) struct BackupRestoration {
    application_commands: ApplicationCommands,
    manager: BackupManager,
}

impl BackupRestoration {
    pub(super) fn new(application_commands: ApplicationCommands, manager: BackupManager) -> Self {
        Self {
            application_commands,
            manager,
        }
    }
}
