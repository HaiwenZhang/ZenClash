use std::{fs, path::PathBuf};

use serde_yaml::{Mapping, Value};

use super::{ControlledConfigError, ControlledConfigResult, ControlledConfigStore};
use crate::profiles::{atomic_write, read_profile_bytes};

pub(super) struct RuntimeCacheTransaction {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    active: bool,
    _write_lease: crate::data_coordinator::DataWriteLease,
}

impl RuntimeCacheTransaction {
    pub(super) fn commit(mut self) {
        self.active = false;
    }

    pub(super) async fn finalize(
        self,
        confirmation: impl std::future::Future<Output = crate::MihomoResult<()>>,
    ) -> ControlledConfigResult<()> {
        // User persistence is already durable. Neither an error nor cancellation may undo it.
        self.commit();
        confirmation.await.map_err(ControlledConfigError::Profile)
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
        let write_lease = self.write_access.acquire();
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
            _write_lease: write_lease,
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
        Ok(home()?
            .join("Library")
            .join("Application Support")
            .join("ZenClash"))
    } else if cfg!(target_os = "windows") {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home()?.join("AppData").join("Local"))
            .join("ZenClash"))
    } else {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local").join("share"))
            .join("zenclash"))
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct CommitGate {
    entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
impl CommitGate {
    pub(crate) fn new() -> (
        std::sync::Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        (
            std::sync::Arc::new(Self {
                entered: parking_lot::Mutex::new(Some(entered)),
                release: parking_lot::Mutex::new(released),
            }),
            waiting,
            release,
        )
    }

    pub(crate) fn wait(&self) {
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.send(());
            self.release.lock().recv().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> ControlledConfigStore {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "zenclash-runtime-finalize-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        ControlledConfigStore::new(root)
    }

    #[tokio::test]
    async fn failed_confirmation_preserves_the_durably_committed_startup_cache() {
        let store = store();
        std::fs::write(store.runtime_path(), "mode: rule\n").unwrap();
        let cache = store.stage_runtime_payload("mode: global\n").unwrap();
        let result = cache
            .finalize(async { Err(crate::MihomoError::RuntimeOutcomeUnknown) })
            .await;
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(store.runtime_path()).unwrap(),
            "mode: global\n"
        );
        std::fs::remove_dir_all(store.root()).unwrap();
    }

    #[tokio::test]
    async fn cancellation_during_confirmation_preserves_the_committed_startup_cache() {
        let store = store();
        std::fs::write(store.runtime_path(), "mode: rule\n").unwrap();
        let cache = store.stage_runtime_payload("mode: global\n").unwrap();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(cache.finalize(async move {
            entered.send(()).unwrap();
            std::future::pending::<crate::MihomoResult<()>>().await
        }));
        waiting.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            std::fs::read_to_string(store.runtime_path()).unwrap(),
            "mode: global\n"
        );
        std::fs::remove_dir_all(store.root()).unwrap();
    }
}
