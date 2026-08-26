use std::{fs, path::PathBuf};

use serde_yaml::{Mapping, Value};

use super::{ControlledConfigError, ControlledConfigResult, ControlledConfigStore};
use crate::profiles::{atomic_write, read_profile_bytes};

pub(super) struct RuntimeCacheTransaction {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    active: bool,
}

impl RuntimeCacheTransaction {
    pub(super) fn commit(mut self) {
        self.active = false;
    }

    pub(super) fn rollback(mut self) -> ControlledConfigResult<()> {
        restore_runtime_cache(&self.path, self.previous.as_deref())?;
        self.active = false;
        Ok(())
    }
}

impl Drop for RuntimeCacheTransaction {
    fn drop(&mut self) {
        if self.active {
            let _ = restore_runtime_cache(&self.path, self.previous.as_deref());
        }
    }
}

impl ControlledConfigStore {
    pub(super) fn stage_runtime_payload(
        &self,
        payload: &str,
    ) -> ControlledConfigResult<RuntimeCacheTransaction> {
        let _transaction = self.transaction.lock();
        let path = self.runtime_path();
        let previous = if path.exists() {
            Some(read_profile_bytes(&path).map_err(|error| match error {
                crate::ProfileStoreError::Io(error) => ControlledConfigError::Io(error),
                _ => ControlledConfigError::TooLarge,
            })?)
        } else {
            None
        };
        atomic_write(&path, payload.as_bytes())?;
        Ok(RuntimeCacheTransaction {
            path,
            previous,
            active: true,
        })
    }

    pub(super) fn load_unlocked(&self) -> ControlledConfigResult<(Option<Vec<u8>>, Value)> {
        let Some(bytes) = self.current_patch_bytes_unlocked()? else {
            return Ok((None, Value::Mapping(Mapping::new())));
        };
        let value = serde_yaml::from_slice(&bytes)?;
        require_mapping(&value)?;
        Ok((Some(bytes), value))
    }

    pub(super) fn current_patch_bytes_unlocked(&self) -> ControlledConfigResult<Option<Vec<u8>>> {
        let path = self.patch_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_profile_bytes(&path).map_err(|error| match error {
            crate::ProfileStoreError::Io(error) => ControlledConfigError::Io(error),
            _ => ControlledConfigError::TooLarge,
        })?;
        Ok(Some(bytes))
    }

    pub(super) fn patch_path(&self) -> PathBuf {
        self.root.join("override.yaml")
    }
}

fn restore_runtime_cache(
    path: &std::path::Path,
    previous: Option<&[u8]>,
) -> ControlledConfigResult<()> {
    if let Some(previous) = previous {
        atomic_write(path, previous)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) fn require_mapping(value: &Value) -> ControlledConfigResult<()> {
    if value.is_mapping() {
        Ok(())
    } else {
        Err(ControlledConfigError::NotMapping)
    }
}

pub(super) fn default_data_dir() -> ControlledConfigResult<PathBuf> {
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or(ControlledConfigError::MissingDataDirectory)
    };
    if cfg!(target_os = "macos") {
        Ok(home()?.join("Library/Application Support/ZenClash"))
    } else if cfg!(target_os = "windows") {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home()?.join("AppData/Local"))
            .join("ZenClash"))
    } else {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/share"))
            .join("zenclash"))
    }
}
