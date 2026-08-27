use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::fs::File;

use super::{CoreUpdateError, CoreUpdateResult};

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
        self.staging
            .as_deref()
            .ok_or_else(|| CoreUpdateError::Io("候选内核已经被使用".into()))
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
        let staging = self
            .staging
            .take()
            .ok_or_else(|| CoreUpdateError::Io("候选内核已经被使用".into()))?;
        let backup = sibling_path(&self.target, "backup")?;
        std::fs::rename(&self.target, &backup).map_err(|error| {
            CoreUpdateError::Io(format!(
                "无法备份 {} 到 {}：{error}",
                self.target.display(),
                backup.display()
            ))
        })?;
        if let Err(error) = std::fs::rename(&staging, &self.target) {
            let restore = std::fs::rename(&backup, &self.target);
            let _ = std::fs::remove_file(&staging);
            return Err(CoreUpdateError::Io(format!(
                "无法启用候选内核：{error}；恢复旧内核结果：{}",
                restore.map_or_else(|restore| restore.to_string(), |()| "成功".into())
            )));
        }
        let transaction = CoreUpdateTransaction {
            target: self.target.clone(),
            backup,
            tag: self.tag.clone(),
            active: true,
        };
        if let Err(error) = sync_parent(&self.target) {
            return match transaction.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => Err(CoreUpdateError::Io(format!(
                    "{error}；并且回滚失败：{rollback}"
                ))),
            };
        }
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
    /// Returns an error when the backup cannot be removed after the new core
    /// has already been accepted. The new core remains active in that case.
    pub fn commit(mut self) -> CoreUpdateResult<()> {
        self.active = false;
        std::fs::remove_file(&self.backup).map_err(|error| {
            CoreUpdateError::Io(format!(
                "新内核已启用，但无法删除备份 {}：{error}",
                self.backup.display()
            ))
        })?;
        sync_parent(&self.target)
    }

    /// Restores the previous executable and removes the rejected candidate.
    ///
    /// Callers must stop any process using the candidate before rollback.
    ///
    /// # Errors
    ///
    /// Returns an error when the active candidate cannot be moved aside or the
    /// backup cannot be restored atomically.
    pub fn rollback(mut self) -> CoreUpdateResult<()> {
        let result = restore_backup(&self.target, &self.backup);
        if self.target.is_file() && !self.backup.exists() {
            self.active = false;
        }
        result
    }

    pub(super) fn preserve_for_manual_recovery(mut self) -> PathBuf {
        self.active = false;
        self.backup.clone()
    }
}

impl Drop for CoreUpdateTransaction {
    fn drop(&mut self) {
        if self.active {
            let _ = restore_backup(&self.target, &self.backup);
        }
    }
}

pub(super) fn sibling_path(target: &Path, label: &str) -> CoreUpdateResult<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| CoreUpdateError::Io(format!("目标没有父目录：{}", target.display())))?;
    let stem = target
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| CoreUpdateError::Io("目标文件名不是有效 UTF-8".into()))?;
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

fn restore_backup(target: &Path, backup: &Path) -> CoreUpdateResult<()> {
    if !backup.is_file() {
        return Err(CoreUpdateError::Io(format!(
            "回滚备份不存在：{}",
            backup.display()
        )));
    }
    let rejected = sibling_path(target, "rejected")?;
    if target.exists() {
        std::fs::rename(target, &rejected).map_err(|error| {
            CoreUpdateError::Io(format!("无法移走候选内核 {}：{error}", target.display()))
        })?;
    }
    if let Err(error) = std::fs::rename(backup, target) {
        if rejected.exists() {
            let _ = std::fs::rename(&rejected, target);
        }
        return Err(CoreUpdateError::Io(format!(
            "无法恢复备份 {}：{error}",
            backup.display()
        )));
    }
    let cleanup = if rejected.exists() {
        std::fs::remove_file(&rejected)
    } else {
        Ok(())
    };
    sync_parent(target)?;
    cleanup
        .map_err(|error| CoreUpdateError::Io(format!("旧内核已恢复，但无法删除候选文件：{error}")))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> CoreUpdateResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreUpdateError::Io("内核路径没有父目录".into()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| CoreUpdateError::Io(format!("同步内核目录失败：{error}")))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> CoreUpdateResult<()> {
    Ok(())
}
