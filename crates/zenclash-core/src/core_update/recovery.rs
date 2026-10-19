//! Durable Local executable replacement, replayed before startup selects a binary.

use super::{
    CoreUpdateError, CoreUpdateResult,
    transaction::{sibling_path, sync_parent},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Deserialize, Serialize)]
enum Phase {
    Preserving,
    Installing,
    Restoring,
    Committed,
    Manual,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateJournal {
    version: u32,
    backup: String,
    staging: String,
    rejected: String,
    phase: Phase,
}

pub(crate) fn recover_interrupted_update(path: &Path) -> CoreUpdateResult<bool> {
    let target = resolve_target(path)?;
    match fs::symlink_metadata(journal_path(&target)?) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(error)),
        Ok(_) => {}
    }
    let access = crate::data_coordinator::DataWriteAccess::new(
        target
            .parent()
            .ok_or_else(|| invalid("core has no parent"))?,
    );
    let _lease = access.acquire().map_err(io_error)?;
    let _lock = lock_target(&target)?;
    recover_locked(&target)
}

pub(super) fn lock_target(target: &Path) -> CoreUpdateResult<crate::AppInstanceLock> {
    crate::AppInstanceLock::acquire(record_path(target, "lock")?)
        .map_err(|error| invalid(error.to_string()))
}

pub(super) fn recover_locked(target: &Path) -> CoreUpdateResult<bool> {
    let Some(mut journal) = UpdateJournal::read(target)? else {
        return Ok(false);
    };
    match journal.phase {
        Phase::Committed => journal.cleanup(target)?,
        Phase::Manual => {
            return Err(invalid(format!(
                "core update requires manual recovery: {}",
                journal.sibling(target, &journal.backup)?.display()
            )));
        }
        _ => journal.rollback(target)?,
    }
    Ok(true)
}

impl UpdateJournal {
    pub(super) fn create(target: &Path, backup: &Path, staging: &Path) -> CoreUpdateResult<Self> {
        let journal = Self {
            version: 1,
            backup: name(backup)?,
            staging: name(staging)?,
            rejected: name(&sibling_path(target, "rejected")?)?,
            phase: Phase::Preserving,
        };
        journal.validate(target)?;
        if fs::canonicalize(
            staging
                .parent()
                .ok_or_else(|| invalid("candidate has no parent"))?,
        )
        .map_err(io_error)?
            != target
                .parent()
                .ok_or_else(|| invalid("core has no parent"))?
        {
            return Err(invalid("core candidate is outside its target directory"));
        }
        journal.save(target)?;
        checkpoint("recorded");
        Ok(journal)
    }

    fn read(target: &Path) -> CoreUpdateResult<Option<Self>> {
        let path = journal_path(target)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
            Ok(metadata) if !metadata.is_file() || metadata.len() > 16 * 1024 => {
                return Err(invalid("invalid core update journal file"));
            }
            Ok(_) => {}
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(io_error)?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > 16 * 1024 {
            return Err(invalid("core update journal exceeds its byte limit"));
        }
        let journal: Self =
            serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
        journal.validate(target)?;
        Ok(Some(journal))
    }

    fn validate(&self, target: &Path) -> CoreUpdateResult<()> {
        for (file, label) in [
            (&self.backup, Some("backup")),
            (&self.staging, None),
            (&self.rejected, Some("rejected")),
        ] {
            let mut components = Path::new(file).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || file.contains(['/', '\\', ':'])
                || !generated_sibling(target, file, label)
            {
                return Err(invalid("unsafe core update journal path"));
            }
        }
        if self.version != 1
            || self.backup == self.staging
            || self.backup == self.rejected
            || self.staging == self.rejected
        {
            return Err(invalid("invalid core update journal version or identities"));
        }
        Ok(())
    }

    fn save(&self, target: &Path) -> CoreUpdateResult<()> {
        let path = journal_path(target)?;
        let bytes = serde_json::to_vec(self).map_err(|error| invalid(error.to_string()))?;
        crate::profiles::atomic_write(&path, &bytes).map_err(io_error)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        sync_parent(target)
    }

    pub(super) fn install(&mut self, target: &Path) -> CoreUpdateResult<()> {
        fs::rename(target, self.sibling(target, &self.backup)?).map_err(io_error)?;
        checkpoint("preserved");
        self.phase = Phase::Installing;
        self.save(target)?;
        fs::rename(self.sibling(target, &self.staging)?, target).map_err(io_error)?;
        sync_parent(target)?;
        checkpoint("installed");
        Ok(())
    }

    pub(super) fn commit(&mut self, target: &Path) -> CoreUpdateResult<()> {
        self.phase = Phase::Committed;
        self.save(target)?;
        checkpoint("committed");
        Ok(())
    }

    pub(super) fn preserve(&mut self, target: &Path) -> CoreUpdateResult<()> {
        self.phase = Phase::Manual;
        self.save(target)
    }

    pub(super) fn rollback(&mut self, target: &Path) -> CoreUpdateResult<()> {
        let backup = self.sibling(target, &self.backup)?;
        if regular_file_exists(&backup)? {
            self.phase = Phase::Restoring;
            self.save(target)?;
            let rejected = self.sibling(target, &self.rejected)?;
            if regular_file_exists(target)? {
                if regular_file_exists(&rejected)? {
                    return Err(invalid("rejected core already exists during recovery"));
                }
                fs::rename(target, rejected).map_err(io_error)?;
            }
            checkpoint("removed-candidate");
            fs::rename(backup, target).map_err(io_error)?;
            sync_parent(target)?;
            checkpoint("restored");
        } else if !matches!(self.phase, Phase::Preserving | Phase::Restoring)
            || !regular_file_exists(target)?
        {
            return Err(invalid("original core is missing during update recovery"));
        }
        self.cleanup(target)
    }

    pub(super) fn cleanup(&self, target: &Path) -> CoreUpdateResult<()> {
        for file in [&self.backup, &self.staging, &self.rejected] {
            let path = self.sibling(target, file)?;
            if regular_file_exists(&path)? {
                fs::remove_file(path).map_err(io_error)?;
                if file == &self.backup {
                    checkpoint("backup-cleaned");
                }
            }
        }
        fs::remove_file(journal_path(target)?).map_err(io_error)?;
        sync_parent(target)
    }

    fn sibling(&self, target: &Path, file: &str) -> CoreUpdateResult<PathBuf> {
        Ok(target
            .parent()
            .ok_or_else(|| invalid("core has no parent"))?
            .join(file))
    }
}

fn generated_sibling(target: &Path, file: &str, expected_label: Option<&str>) -> bool {
    let Some(stem) = target.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let prefix = format!(".{stem}.zenclash-");
    if !file
        .get(..prefix.len())
        .is_some_and(|value| value.eq_ignore_ascii_case(&prefix))
    {
        return false;
    }
    let mut suffix = &file[prefix.len()..];
    if let Some(extension) = target.extension().and_then(|extension| extension.to_str()) {
        let extension = format!(".{extension}");
        let Some(boundary) = suffix.len().checked_sub(extension.len()) else {
            return false;
        };
        if !suffix
            .get(boundary..)
            .is_some_and(|value| value.eq_ignore_ascii_case(&extension))
        {
            return false;
        }
        suffix = &suffix[..boundary];
    }
    let mut parts = suffix.rsplitn(3, '-');
    let sequence = parts
        .next()
        .is_some_and(|value| value.parse::<u64>().is_ok());
    let pid = parts
        .next()
        .is_some_and(|value| value.parse::<u32>().is_ok());
    let label = parts.next().is_some_and(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && expected_label.is_none_or(|expected| expected == label)
    });
    sequence && pid && label
}

fn regular_file_exists(path: &Path) -> CoreUpdateResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(invalid(format!(
            "core update path is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(error)),
    }
}

pub(super) fn resolve_target(path: &Path) -> CoreUpdateResult<PathBuf> {
    let mut path = std::path::absolute(path).map_err(io_error)?;
    // A configured symlink can be dangling while the old core is in its backup.
    for _ in 0..40 {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let link = fs::read_link(&path).map_err(io_error)?;
                path = if link.is_absolute() {
                    link
                } else {
                    path.parent()
                        .ok_or_else(|| invalid("linked core has no parent"))?
                        .join(link)
                };
            }
            Ok(_) => return resolve_parent(&path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return resolve_parent(&path),
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(invalid("too many symbolic links in core path"))
}

fn resolve_parent(path: &Path) -> CoreUpdateResult<PathBuf> {
    let parent = path.parent().ok_or_else(|| invalid("core has no parent"))?;
    let parent = match fs::canonicalize(parent) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(error) => return Err(io_error(error)),
    };
    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| invalid("core has no name"))?,
    ))
}

fn name(path: &Path) -> CoreUpdateResult<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid("core update filename is not UTF-8"))
}

fn record_path(target: &Path, extension: &str) -> CoreUpdateResult<PathBuf> {
    Ok(target.with_file_name(format!(".{}.zenclash-update.{extension}", name(target)?)))
}

fn journal_path(target: &Path) -> CoreUpdateResult<PathBuf> {
    record_path(target, "json")
}
fn invalid(message: impl Into<String>) -> CoreUpdateError {
    CoreUpdateError::Io(message.into())
}
fn io_error(error: io::Error) -> CoreUpdateError {
    invalid(error.to_string())
}

pub(super) fn checkpoint(_phase: &str) {
    #[cfg(test)]
    if std::env::var("ZENCLASH_CORE_UPDATE_CRASH_STEP")
        .ok()
        .as_deref()
        == Some(_phase)
    {
        std::process::exit(86);
    }
}

#[cfg(test)]
mod tests;
