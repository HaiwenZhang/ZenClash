use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_yaml::{Mapping, Value};

use super::{
    ControlledConfigError, ControlledConfigResult, ControlledConfigStore, ControlledConfigUpdate,
};
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
        let write_lease = self.write_access.acquire()?;
        let _transaction = self.transaction.lock();
        let path = self.runtime_path();
        let previous = read_optional_payload(&path)?;
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
        read_optional_payload(&self.patch_path())
    }

    pub(super) fn patch_path(&self) -> PathBuf {
        self.root.join("override.yaml")
    }
}

pub(super) fn read_optional_payload(
    path: &std::path::Path,
) -> ControlledConfigResult<Option<Vec<u8>>> {
    match read_profile_bytes(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(crate::ProfileStoreError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(crate::ProfileStoreError::Io(error)) => Err(ControlledConfigError::Io(error)),
        Err(_) => Err(ControlledConfigError::TooLarge),
    }
}

fn restore_runtime_cache(
    path: &std::path::Path,
    previous: Option<&[u8]>,
) -> ControlledConfigResult<()> {
    if let Some(previous) = previous {
        atomic_write(path, previous)?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ControlledConfigError::Io(error)),
        }
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

impl ControlledConfigStore {
    /// Loads the current YAML override mapping.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, oversized, malformed, or non-mapping
    /// content.
    pub fn load(&self) -> ControlledConfigResult<Value> {
        let _transaction = self.transaction.lock();
        self.load_unlocked().map(|(_, value)| value)
    }

    /// Loads the current override as JSON for native UI state.
    ///
    /// # Errors
    ///
    /// Returns an error when the YAML layer is invalid or cannot be represented
    /// as a JSON object.
    pub fn load_json(&self) -> ControlledConfigResult<serde_json::Value> {
        Ok(serde_json::to_value(self.load()?)?)
    }

    /// Persists a prepared update if the controlled layer has not changed.
    ///
    /// # Errors
    ///
    /// Returns an error for concurrent modification or an atomic write failure.
    pub fn commit(&self, update: &ControlledConfigUpdate) -> ControlledConfigResult<()> {
        #[cfg(test)]
        if let Some(gate) = &self.commit_gate {
            gate.wait();
        }
        let _write_lease = self.write_access.acquire()?;
        let _transaction = self.transaction.lock();
        if self.current_patch_bytes_unlocked()? != update.expected_patch {
            return Err(ControlledConfigError::ConcurrentModification);
        }
        atomic_write(&self.patch_path(), &update.next_patch)?;
        Ok(())
    }

    /// Returns the generated runtime-cache path.
    #[must_use]
    pub fn runtime_path(&self) -> PathBuf {
        self.root.join("effective.yaml")
    }

    /// Returns the controlled-config storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Quarantines a malformed persisted override and restores an empty layer.
    ///
    /// Only parse, shape, and defensive-size failures are recoverable. Native
    /// I/O errors are returned unchanged so permission and disk failures are
    /// never mistaken for successful repair.
    ///
    /// # Errors
    ///
    /// Returns the original non-recoverable error or a filesystem error while
    /// moving the malformed document to its timestamped backup path.
    pub fn quarantine_invalid_patch(&self) -> ControlledConfigResult<Option<PathBuf>> {
        let _write_lease = self.write_access.acquire()?;
        let _transaction = self.transaction.lock();
        let error = match self.load_unlocked() {
            Ok(_) => return Ok(None),
            Err(error) => error,
        };
        if !matches!(
            error,
            ControlledConfigError::Yaml(_)
                | ControlledConfigError::NotMapping
                | ControlledConfigError::TooLarge
        ) {
            return Err(error);
        }
        let source = self.patch_path();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine = self.root.join(format!(
            "override.invalid-{}-{timestamp}.yaml",
            std::process::id()
        ));
        fs::rename(&source, &quarantine)?;
        Ok(Some(quarantine))
    }

    pub(crate) fn cached_runtime_payload(&self) -> ControlledConfigResult<Option<String>> {
        let Some(bytes) = read_optional_payload(&self.runtime_path())? else {
            return Ok(None);
        };
        String::from_utf8(bytes).map(Some).map_err(|error| {
            ControlledConfigError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
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
