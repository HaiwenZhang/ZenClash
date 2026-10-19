use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::fs::File;

use super::{
    CoreUpdateError, CoreUpdateResult,
    recovery::{self, UpdateJournal},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Validated same-directory candidate that has not replaced the active core.
pub struct PreparedCoreUpdate {
    pub(super) staging: Option<PathBuf>,
    pub(super) target: PathBuf,
    pub(super) tag: String,
}

impl PreparedCoreUpdate {
    /// Returns the version tag validated from the candidate's `-v` output.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub(super) fn candidate_path(&self) -> CoreUpdateResult<&Path> {
        self.staging.as_deref().ok_or_else(|| {
            CoreUpdateError::Io(zenclash_i18n::text(
                "core_update.details.candidate_consumed",
            ))
        })
    }

    /// Atomically moves the active core to a backup and installs the candidate.
    ///
    /// Callers should stop the managed process immediately before activation,
    /// start and verify the new binary, then call [`CoreUpdateTransaction::commit`].
    /// Dropping the returned transaction without committing restores the backup.
    ///
    /// # Errors
    ///
    /// Returns an error when the current core cannot be backed up, the staging
    /// file cannot be renamed into place, or the directory cannot be synced.
    pub fn activate(mut self) -> CoreUpdateResult<CoreUpdateTransaction> {
        let staging = self.candidate_path()?.to_path_buf();
        let target = recovery::resolve_target(&self.target)?;
        if !target.is_file() || !staging.is_file() {
            return Err(CoreUpdateError::Io(
                "core update requires existing executable files".into(),
            ));
        }
        let lock = recovery::lock_target(&target)?;
        recovery::recover_locked(&target)?;
        let backup = sibling_path(&target, "backup")?;
        let journal = UpdateJournal::create(&target, &backup, &staging)?;
        let mut transaction = CoreUpdateTransaction {
            target,
            backup,
            tag: self.tag.clone(),
            active: true,
            journal,
            _lock: lock,
        };
        if let Err(error) = transaction.journal.install(&transaction.target) {
            return match transaction.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => Err(CoreUpdateError::Io(format!(
                    "{error}; core rollback failed: {rollback}"
                ))),
            };
        }
        self.staging = None;
        Ok(transaction)
    }
}

impl Drop for PreparedCoreUpdate {
    fn drop(&mut self) {
        if let Some(staging) = self.staging.take() {
            let _ = std::fs::remove_file(staging);
        }
    }
}

/// Activated replacement that rolls back automatically until committed.
pub struct CoreUpdateTransaction {
    target: PathBuf,
    backup: PathBuf,
    tag: String,
    active: bool,
    journal: UpdateJournal,
    _lock: crate::AppInstanceLock,
}

impl CoreUpdateTransaction {
    /// Returns the installed version tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Keeps the new core and removes the previous executable backup.
    ///
    /// # Errors
    ///
    /// Returns an error when acceptance cannot be recorded or backup cleanup fails.
    /// The candidate remains in place; an unrecorded commit retains the old backup
    /// for startup recovery rather than replacing a potentially running executable.
    pub fn commit(mut self) -> CoreUpdateResult<()> {
        if let Err(error) = self.record_commit() {
            // The verified candidate may still be running; retain files for startup recovery.
            self.active = false;
            return Err(error);
        }
        self.journal.cleanup(&self.target)
    }

    pub(crate) fn record_commit(&mut self) -> CoreUpdateResult<()> {
        if self.active {
            // Commit follows live-core verification; only the workflow may stop and roll it back.
            self.active = false;
            self.journal.commit(&self.target)?;
        }
        Ok(())
    }

    /// Restores the previous executable and removes the rejected candidate.
    ///
    /// Callers must stop any process using the candidate before rollback.
    ///
    /// # Errors
    ///
    /// Returns an error when the active candidate cannot be moved aside or the
    /// backup cannot be restored atomically.
    pub fn rollback(self) -> CoreUpdateResult<()> {
        self.rollback_with_status().0
    }

    pub(crate) fn rollback_with_status(mut self) -> (CoreUpdateResult<()>, bool) {
        let result = self.journal.rollback(&self.target);
        let restored = self.target.is_file() && !self.backup.exists();
        if restored {
            self.active = false;
        }
        (result, restored)
    }

    pub(super) fn preserve_for_manual_recovery(mut self) -> CoreUpdateResult<PathBuf> {
        self.active = false;
        self.journal.preserve(&self.target).map_err(|error| {
            CoreUpdateError::Io(format!(
                "{error}; previous executable retained at {}",
                self.backup.display()
            ))
        })?;
        Ok(self.backup.clone())
    }
}

impl Drop for CoreUpdateTransaction {
    fn drop(&mut self) {
        if self.active {
            let _ = self.journal.rollback(&self.target);
        }
    }
}

pub(super) fn sibling_path(target: &Path, label: &str) -> CoreUpdateResult<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        CoreUpdateError::Io(zenclash_i18n::text_with(
            "core_update.details.target_parent",
            &[("path", target.display().to_string())],
        ))
    })?;
    let stem = target
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            CoreUpdateError::Io(zenclash_i18n::text("core_update.details.target_name_utf8"))
        })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = target
        .extension()
        .and_then(|value| value.to_str())
        .map_or_else(
            || format!(".{stem}.zenclash-{label}-{}-{sequence}", std::process::id()),
            |extension| {
                format!(
                    ".{stem}.zenclash-{label}-{}-{sequence}.{extension}",
                    std::process::id()
                )
            },
        );
    Ok(parent.join(name))
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> CoreUpdateResult<()> {
    let parent = path.parent().ok_or_else(|| {
        CoreUpdateError::Io(zenclash_i18n::text("core_update.details.core_parent"))
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            CoreUpdateError::Io(zenclash_i18n::text_with(
                "core_update.details.sync_directory",
                &[("error", error.to_string())],
            ))
        })
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_path: &Path) -> CoreUpdateResult<()> {
    Ok(())
}
