//! Write-ahead restore state, replayed before startup opens authoritative stores.

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{BackupError, BackupResult, transaction::LIVE_ITEMS};

const JOURNAL_FILE: &str = ".backup-restore.json";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum ItemState {
    Untouched,
    Preserving,
    Installing,
    Restoring,
    Restored,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreJournal {
    version: u32,
    rollback_name: String,
    staging_name: String,
    original: [bool; 4],
    states: [ItemState; 4],
    remove_empty_data_root: bool,
    committed: bool,
}

impl RestoreJournal {
    pub(super) fn create(
        root: &Path,
        rollback: &Path,
        staging: &Path,
        remove_empty_data_root: bool,
    ) -> BackupResult<Self> {
        if let Some(previous) = Self::read(root)? {
            if !previous.committed {
                return Err(invalid("an unfinished backup restore requires recovery"));
            }
            previous.cleanup(root)?;
        }
        let mut original = [false; 4];
        for (index, item) in LIVE_ITEMS.iter().enumerate() {
            original[index] = exists(&root.join(item))?;
        }
        let journal = Self {
            version: 1,
            rollback_name: directory_name(rollback)?,
            staging_name: directory_name(staging)?,
            original,
            states: [ItemState::Untouched; 4],
            remove_empty_data_root,
            committed: false,
        };
        journal.save(root)?;
        Ok(journal)
    }

    pub(super) fn read(root: &Path) -> BackupResult<Option<Self>> {
        let path = root.join(JOURNAL_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.len() > 16 * 1024 {
            return Err(invalid("invalid backup restore journal file"));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 {
            return Err(invalid("backup restore journal exceeds its size limit"));
        }
        let journal: Self = serde_json::from_slice(&bytes)?;
        if journal.version != 1
            || !generated_name(&journal.rollback_name, ".zenclash-backup-rollback-")
            || !generated_name(&journal.staging_name, ".zenclash-backup-staging-")
        {
            return Err(invalid("invalid backup restore journal paths or version"));
        }
        Ok(Some(journal))
    }

    fn save(&self, root: &Path) -> BackupResult<()> {
        let path = root.join(JOURNAL_FILE);
        crate::profiles::atomic_write(&path, &serde_json::to_vec(self)?)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?
            .sync_all()?;
        #[cfg(unix)]
        fs::File::open(root)?.sync_all()?;
        Ok(())
    }

    pub(super) fn install(&mut self, root: &Path, index: usize) -> BackupResult<()> {
        let item = LIVE_ITEMS[index];
        self.states[index] = ItemState::Preserving;
        self.save(root)?;
        checkpoint("before-preserve", item);
        if self.original[index] {
            fs::rename(root.join(item), self.rollback_root(root)?.join(item))?;
        }
        checkpoint("preserved", item);
        self.states[index] = ItemState::Installing;
        self.save(root)?;
        fs::rename(self.staging_root(root)?.join(item), root.join(item))?;
        checkpoint("installed", item);
        Ok(())
    }

    pub(super) fn rollback(&mut self, root: &Path) -> BackupResult<()> {
        self.committed = false;
        for (index, item) in LIVE_ITEMS.iter().enumerate().rev() {
            let state = self.states[index];
            if matches!(state, ItemState::Untouched | ItemState::Restored) {
                continue;
            }
            let live = root.join(item);
            let previous = self.rollback_root(root)?.join(item);
            if self.original[index] && !exists(&previous)? {
                // The preserve rename has not happened, or the restore rename already finished.
                if !matches!(state, ItemState::Preserving | ItemState::Restoring) || !exists(&live)?
                {
                    return Err(invalid(format!("missing original backup item {item}")));
                }
            } else {
                self.states[index] = ItemState::Restoring;
                self.save(root)?;
                remove_path(&live)?;
                checkpoint("removed", item);
                if self.original[index] {
                    fs::rename(previous, live)?;
                }
                checkpoint("restored", item);
            }
            self.states[index] = ItemState::Restored;
            self.save(root)?;
        }
        self.cleanup(root)
    }

    pub(super) fn commit(&mut self, root: &Path) -> BackupResult<()> {
        self.committed = true;
        if let Err(error) = self.save(root) {
            self.committed = false;
            return Err(error);
        }
        checkpoint("committed", "all");
        Ok(())
    }

    pub(super) fn cleanup(&self, root: &Path) -> BackupResult<()> {
        remove_private_directory(&self.rollback_root(root)?)?;
        remove_private_directory(&self.staging_root(root)?)?;
        match fs::remove_file(root.join(JOURNAL_FILE)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if !self.committed
            && self.remove_empty_data_root
            && root.exists()
            && fs::read_dir(root)?.next().is_none()
        {
            fs::remove_dir(root)?;
        }
        Ok(())
    }

    pub(super) fn recover(&mut self, root: &Path) -> BackupResult<()> {
        if self.committed {
            self.cleanup(root)
        } else {
            self.rollback(root)
        }
    }

    fn rollback_root(&self, root: &Path) -> BackupResult<PathBuf> {
        sibling(root, &self.rollback_name)
    }

    fn staging_root(&self, root: &Path) -> BackupResult<PathBuf> {
        sibling(root, &self.staging_name)
    }
}

fn sibling(root: &Path, name: &str) -> BackupResult<PathBuf> {
    Ok(root
        .parent()
        .ok_or(BackupError::MissingDataDirectory)?
        .join(name))
}

fn directory_name(path: &Path) -> BackupResult<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid("backup transaction directory name is not UTF-8"))
}

fn generated_name(name: &str, prefix: &str) -> bool {
    let Some((pid, sequence)) = name
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.split_once('-'))
    else {
        return false;
    };
    pid.parse::<u32>().is_ok() && sequence.parse::<u64>().is_ok()
}

fn exists(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_private_directory(path: &Path) -> BackupResult<()> {
    if exists(path)? {
        if !fs::symlink_metadata(path)?.is_dir() {
            return Err(invalid(
                "backup transaction directory is not a real directory",
            ));
        }
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn invalid(message: impl Into<String>) -> BackupError {
    BackupError::Transaction(message.into())
}

fn checkpoint(_phase: &str, _item: &str) {
    #[cfg(test)]
    if std::env::var("ZENCLASH_BACKUP_CRASH_STEP").ok().as_deref()
        == Some(&format!("{_phase}:{_item}"))
    {
        std::process::exit(86);
    }
}
