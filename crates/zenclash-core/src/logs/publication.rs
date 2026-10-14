//! Generation transitions and log stream/buffer/persistence admission.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use super::{
    BufferedLogEntry, LogBuffer, LogEntry, LogMonitor, LogStreamFormat, LogStreamSnapshot, file,
    now_ms, push_prepared_bounded,
};

impl LogMonitor {
    /// Changes the accepted core generation and reconnects the WebSocket.
    ///
    /// Frames already queued by the older socket are rejected before they can
    /// enter the visible log buffer.
    pub fn synchronize_generation(&self, generation: u64) {
        let mut stream = self.stream.write();
        if self.expected_generation.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        stream.connected = false;
        self.generation.send_replace(generation);
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) fn update_log_connection(
    stream: &RwLock<LogStreamSnapshot>,
    connected: bool,
    error: Option<String>,
    generation: u64,
    expected_generation: &AtomicU64,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
    );
    let mut stream = stream.write();
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    if stream.generation != generation {
        *stream = LogStreamSnapshot {
            generation,
            ..LogStreamSnapshot::default()
        };
    }
    stream.connected = connected;
    if connected && error.is_none() {
        stream.last_error = None;
    }
    if let Some(error) = error {
        stream.last_error = Some(error);
    }
    true
}

pub(super) fn accept_log_entry(
    entries: &RwLock<LogBuffer>,
    stream: &RwLock<LogStreamSnapshot>,
    revision: &AtomicU64,
    file_sender: &file::LogFileSender,
    entry: LogEntry,
    generation: u64,
    expected_generation: &AtomicU64,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let Some(entry) = BufferedLogEntry::prepare(entry) else {
        push_monitor_error_for_generation(
            entries,
            stream,
            revision,
            file_sender,
            zenclash_i18n::text("logs.errors.entry_too_large"),
            generation,
            expected_generation,
        );
        return false;
    };
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
    );
    let mut stream = stream.write();
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    if stream.generation != generation {
        return false;
    }
    stream.connected = true;
    stream.updated_at_ms = if entry.entry.received_at_ms == 0 {
        entry.entry.timestamp_ms
    } else {
        entry.entry.received_at_ms
    };
    stream.last_error = None;
    stream.format = if entry.entry.core_time.is_some() || !entry.entry.fields.is_null() {
        LogStreamFormat::Structured
    } else {
        LogStreamFormat::Plain
    };
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeBufferAppend,
    );
    // Keep the stream gate through the bounded buffer write and nonblocking queue admission.
    let entry = push_prepared_bounded(entries, entry);
    file_sender.append(entry.entry, entry.bytes);
    revision.fetch_add(1, Ordering::AcqRel);
    drop(stream);
    true
}

pub(super) fn push_monitor_error_for_generation(
    entries: &RwLock<LogBuffer>,
    stream: &RwLock<LogStreamSnapshot>,
    revision: &AtomicU64,
    file_sender: &file::LogFileSender,
    payload: String,
    generation: u64,
    expected_generation: &AtomicU64,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let last_error = payload.clone();
    let entry = BufferedLogEntry::prepare(LogEntry {
        level: "error".into(),
        payload,
        timestamp_ms: now_ms(),
        ..LogEntry::default()
    });
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
    );
    let mut stream = stream.write();
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    if stream.generation != generation {
        *stream = LogStreamSnapshot {
            generation,
            ..LogStreamSnapshot::default()
        };
    }
    stream.last_error = Some(last_error);
    let Some(entry) = entry else {
        return false;
    };
    let entry = push_prepared_bounded(entries, entry);
    file_sender.append(entry.entry, entry.bytes);
    revision.fetch_add(1, Ordering::AcqRel);
    drop(stream);
    true
}

#[cfg(test)]
mod tests;
