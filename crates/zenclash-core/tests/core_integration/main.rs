//! Fast, zero-external-binary black-box integration tests across all five layers
//! of `zenclash-core`'s public API.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

mod config_pipeline;
mod runtime_reconciler;
mod storage_transactions;
mod telemetry_and_pac;

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Self-cleaning temporary directory for isolated integration tests.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(prefix: &str) -> std::io::Result<Self> {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zenclash-core-int-{prefix}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
