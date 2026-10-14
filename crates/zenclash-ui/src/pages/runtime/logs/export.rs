//! Background formatting and private file export for accepted log snapshots.
use std::sync::Arc;
use zenclash_core::{format_log_entries, format_log_entries_support_safe};

pub(super) async fn export_log_entries(
    path: std::path::PathBuf,
    entries: Vec<Arc<zenclash_core::LogEntry>>,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let payload = prepare_log_payload(entries, false);
        zenclash_core::write_log_export(&path, &payload)
    })
    .await
    .map_err(std::io::Error::other)?
}

pub(super) fn prepare_log_payload(
    entries: Vec<Arc<zenclash_core::LogEntry>>,
    support_safe: bool,
) -> String {
    let entries = entries
        .into_iter()
        .map(|entry| entry.as_ref().clone())
        .collect::<Vec<_>>();
    if support_safe {
        format_log_entries_support_safe(&entries)
    } else {
        format_log_entries(&entries)
    }
}
