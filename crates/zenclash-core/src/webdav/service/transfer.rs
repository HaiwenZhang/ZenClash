use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use super::super::WebDavResult;

static TRANSFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct TransferFile {
    pub(super) path: PathBuf,
}

impl TransferFile {
    pub(super) fn new(data_root: &Path, filename: &str) -> WebDavResult<Self> {
        let directory = data_root.join(".webdav-transfers");
        fs::create_dir_all(&directory)?;
        let sequence = TRANSFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "{}.{}.{}.tmp",
            filename,
            std::process::id(),
            sequence
        ));
        Ok(Self { path })
    }
}

impl Drop for TransferFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %self.path.display(), "failed to remove WebDAV transfer file");
            }
        }
    }
}

pub(super) fn backup_filename() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let sequence = TRANSFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp:010}-{sequence:06}.zip", backup_prefix())
}

pub(super) fn backup_prefix() -> String {
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-device".into());
    format!(
        "zenclash-{}-{}",
        std::env::consts::OS,
        sanitize_device_name(&hostname)
    )
}

fn sanitize_device_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            ) || character.is_whitespace()
            {
                '-'
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = sanitized
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if sanitized.is_empty() {
        "unknown-device".into()
    } else {
        sanitized
    }
}
