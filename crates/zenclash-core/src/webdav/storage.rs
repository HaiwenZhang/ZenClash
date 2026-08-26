use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;

use super::{WebDavError, WebDavResult, WebDavSettings};
use crate::{profiles::atomic_write, AppPreferencesStore};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

/// Atomic local store for `WebDAV` connection settings.
#[derive(Clone, Debug)]
pub struct WebDavSettingsStore {
    path: PathBuf,
    transaction: Arc<Mutex<()>>,
}

impl WebDavSettingsStore {
    /// Opens `webdav.json` below the platform-default `ZenClash` data root.
    ///
    /// # Errors
    ///
    /// Returns an error when the application-data directory cannot be found.
    pub fn discover() -> WebDavResult<Self> {
        let preferences = AppPreferencesStore::discover().map_err(|error| {
            WebDavError::InvalidSettings(format!("无法确定应用数据目录：{error}"))
        })?;
        let root = preferences
            .path()
            .parent()
            .ok_or_else(|| WebDavError::InvalidSettings("应用设置路径没有父目录".into()))?;
        Ok(Self::new(root.join("webdav.json")))
    }

    /// Creates a settings store backed by an explicit JSON path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            transaction: Arc::new(Mutex::new(())),
        }
    }

    /// Loads settings, returning defaults when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures, oversized input, or invalid JSON.
    pub fn load(&self) -> WebDavResult<WebDavSettings> {
        let _transaction = self.transaction.lock();
        if !self.path.exists() {
            return Ok(WebDavSettings::default());
        }
        let file = fs::File::open(&self.path)?;
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(WebDavError::ResponseTooLarge(
                "本地 webdav.json 超过 1 MiB".into(),
            ));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Atomically saves settings in a private file on Unix.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or the private atomic write fails.
    pub fn save(&self, settings: &WebDavSettings) -> WebDavResult<()> {
        let _transaction = self.transaction.lock();
        let bytes = serde_json::to_vec_pretty(settings)?;
        atomic_write(&self.path, &bytes)?;
        Ok(())
    }

    /// Returns the JSON path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
