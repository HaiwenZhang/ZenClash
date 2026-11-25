use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Days, Local, NaiveTime, TimeZone};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    MihomoClient, MihomoEndpoint,
    runtime::client::{ControllerBinding, ControllerStream},
};

mod file;
mod private_io;

mod publication;
use publication::{accept_log_entry, push_monitor_error_for_generation, update_log_connection};

pub use file::{LogPersistenceError, LogPersistenceResult, LogPersistenceStatus};

const MAX_LOG_ENTRIES: usize = 500;
const MAX_LOG_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_LOG_ENTRY_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct LogBuffer {
    entries: VecDeque<BufferedLogEntry>,
    bytes: usize,
}

#[derive(Clone)]
struct BufferedLogEntry {
    entry: Arc<LogEntry>,
    bytes: usize,
}

impl BufferedLogEntry {
    fn prepare(entry: LogEntry) -> Option<Self> {
        let bytes = log_entry_bytes(&entry);
        (bytes <= MAX_LOG_ENTRY_BYTES).then(|| Self {
            entry: Arc::new(entry),
            bytes,
        })
    }
}

fn log_entry_bytes(entry: &LogEntry) -> usize {
    const FIXED_ENTRY_OVERHEAD: usize = 64;
    FIXED_ENTRY_OVERHEAD
        .saturating_add(entry.level.len())
        .saturating_add(entry.payload.len())
        .saturating_add(entry.core_time.as_deref().map_or(0, str::len))
        .saturating_add(json_value_bytes(&entry.fields))
}

fn json_value_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.to_string().len(),
        Value::String(text) => text.len().saturating_add(2),
        Value::Array(items) => items.iter().fold(2usize, |acc, item| {
            acc.saturating_add(json_value_bytes(item)).saturating_add(1)
        }),
        Value::Object(map) => map.iter().fold(2usize, |acc, (key, val)| {
            acc.saturating_add(key.len())
                .saturating_add(4)
                .saturating_add(json_value_bytes(val))
        }),
    }
}

/// Severity threshold accepted by Mihomo's `/logs` stream.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MihomoLogLevel {
    /// Disable core log events.
    Silent,
    /// Keep only errors.
    Error,
    /// Keep warnings and errors.
    Warning,
    /// Keep normal operational events, warnings, and errors.
    #[default]
    Info,
    /// Include verbose diagnostic events.
    Debug,
}

impl MihomoLogLevel {
    /// Parses a Mihomo runtime configuration value.
    #[must_use]
    pub fn from_api(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "silent" => Some(Self::Silent),
            "error" => Some(Self::Error),
            "warning" | "warn" => Some(Self::Warning),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    /// Returns the query value accepted by Mihomo.
    #[must_use]
    pub const fn api_value(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

/// One entry received from Mihomo's `/logs` stream.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LogEntry {
    /// Mihomo log severity.
    #[serde(default, rename = "type", alias = "level")]
    pub level: String,
    /// Human-readable Mihomo log payload.
    #[serde(default, alias = "message")]
    pub payload: String,
    /// Original timestamp supplied by a structured Mihomo frame.
    #[serde(default, rename = "time")]
    pub core_time: Option<String>,
    /// Structured Mihomo fields retained without flattening them into text.
    #[serde(default)]
    pub fields: Value,
    /// Normalized display time as Unix milliseconds.
    #[serde(default)]
    pub timestamp_ms: u64,
    /// Local receive time used for stream freshness and reconnect observations.
    #[serde(default)]
    pub received_at_ms: u64,
    /// Source used for [`Self::timestamp_ms`].
    #[serde(default)]
    pub time_source: LogTimeSource,
}

impl LogEntry {
    /// Returns only caller-selected structured fields.
    ///
    /// This is a presentation filter, not a redaction policy. Support exports
    /// should use [`format_log_entries_support_safe`] instead.
    #[must_use]
    pub fn filtered_fields(&self, allowed: &[&str]) -> BTreeMap<String, Value> {
        let Some(fields) = self.fields.as_object() else {
            return BTreeMap::new();
        };
        allowed
            .iter()
            .filter_map(|key| {
                fields
                    .get(*key)
                    .cloned()
                    .map(|value| ((*key).to_owned(), value))
            })
            .collect()
    }
}

/// Origin of a log entry's normalized display timestamp.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LogTimeSource {
    /// Parsed from Mihomo's structured `time` field.
    Core,
    /// Captured locally because the frame had no usable core timestamp.
    #[default]
    LocalReceive,
}

/// Connection health and generation of the `/logs` stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogStreamSnapshot {
    /// Core-session generation accepted by the monitor.
    pub generation: u64,
    /// Whether the current generation's WebSocket is connected.
    pub connected: bool,
    /// Unix timestamp in milliseconds of the last accepted log frame.
    pub updated_at_ms: u64,
    /// Most recent transport or frame error.
    pub last_error: Option<String>,
    /// Format observed from the most recently accepted frame.
    pub format: LogStreamFormat,
}

/// Wire format observed on Mihomo's `/logs` stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogStreamFormat {
    /// No frame has been accepted for the current generation.
    #[default]
    Unknown,
    /// Mihomo structured frame with core time and typed fields.
    Structured,
    /// Legacy plain frame using local receive time.
    Plain,
}

/// Maintains a bounded, reconnecting Mihomo `/logs` stream.
pub struct LogMonitor {
    entries: Arc<RwLock<LogBuffer>>,
    stream: Arc<RwLock<LogStreamSnapshot>>,
    revision: Arc<AtomicU64>,
    level: watch::Sender<MihomoLogLevel>,
    expected_generation: Arc<AtomicU64>,
    generation: watch::Sender<u64>,
    file: file::LogFileWorker,
    task: JoinHandle<()>,
}

impl LogMonitor {
    /// Starts a log monitor at the requested Mihomo severity level.
    ///
    /// The requested threshold is preserved, including Debug.
    #[must_use]
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint, level: MihomoLogLevel) -> Arc<Self> {
        Self::start_binding(runtime, ControllerBinding::direct(endpoint), level)
    }

    /// Starts a monitor following the client's Direct or native Service transport.
    #[must_use]
    pub fn start_with_client(
        runtime: &Handle,
        client: MihomoClient,
        level: MihomoLogLevel,
    ) -> Arc<Self> {
        Self::start_binding(runtime, client.binding, level)
    }

    fn start_binding(
        runtime: &Handle,
        binding: Arc<ControllerBinding>,
        level: MihomoLogLevel,
    ) -> Arc<Self> {
        let entries = Arc::new(RwLock::new(LogBuffer::default()));
        let stream = Arc::new(RwLock::new(LogStreamSnapshot::default()));
        let revision = Arc::new(AtomicU64::new(0));
        let file = file::LogFileWorker::start();
        let (level_sender, level_receiver) = watch::channel(level);
        let expected_generation = Arc::new(AtomicU64::new(0));
        let (generation, generation_receiver) = watch::channel(0);
        let task = runtime.spawn(run_monitor(
            binding,
            level_receiver,
            generation_receiver,
            LogMonitorState {
                entries: entries.clone(),
                stream: stream.clone(),
                revision: revision.clone(),
                expected_generation: expected_generation.clone(),
                file_sender: file.sender(),
            },
        ));
        Arc::new(Self {
            entries,
            stream,
            revision,
            level: level_sender,
            expected_generation,
            generation,
            file,
            task,
        })
    }

    /// Returns the currently buffered log entries in receive order.
    #[must_use]
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries
            .read()
            .entries
            .iter()
            .map(|buffered| buffered.entry.as_ref().clone())
            .collect()
    }

    /// Returns reference-counted entries for read-only UI presentation without
    /// cloning every payload and structured field.
    #[must_use]
    pub fn shared_entries(&self) -> Vec<Arc<LogEntry>> {
        self.entries
            .read()
            .entries
            .iter()
            .map(|buffered| Arc::clone(&buffered.entry))
            .collect()
    }

    /// Returns the number of currently buffered entries without cloning them.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.read().entries.len()
    }

    /// Removes all currently buffered log entries.
    pub fn clear(&self) {
        let mut entries = self.entries.write();
        if !entries.entries.is_empty() {
            let previous = std::mem::take(&mut *entries);
            drop(entries);
            self.revision.fetch_add(1, Ordering::AcqRel);
            drop(previous);
        }
    }

    /// Returns a monotonic revision for visible entries and stream connection state.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Changes the stream threshold and reconnects only when it actually differs.
    pub fn set_level(&self, level: MihomoLogLevel) {
        if *self.level.borrow() != level {
            self.level.send_replace(level);
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Returns the active or next requested stream threshold.
    #[must_use]
    pub fn level(&self) -> MihomoLogLevel {
        *self.level.borrow()
    }

    /// Configures bounded, continuous persistence for newly received log entries.
    ///
    /// A disabled configuration retains the target path and limit for status
    /// display but performs no writes. Configuration changes take effect without
    /// reconnecting the Mihomo stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the file-size limit is outside 1–100 MiB.
    pub fn configure_persistence(
        &self,
        path: impl Into<PathBuf>,
        enabled: bool,
        max_mebibytes: u16,
    ) -> LogPersistenceResult<()> {
        self.file.configure(path.into(), enabled, max_mebibytes)
    }

    /// Returns a snapshot of the persistent-log writer state.
    #[must_use]
    pub fn persistence_status(&self) -> LogPersistenceStatus {
        self.file.status()
    }

    /// Returns entries awaiting completion of a disk write, including the in-flight entry.
    #[must_use]
    pub fn pending_persistence_entries(&self) -> usize {
        self.file.pending_entries()
    }

    /// Returns whether the log WebSocket is currently connected.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.stream.read().connected
    }

    /// Returns the current stream generation, freshness, and transport error.
    #[must_use]
    pub fn stream_snapshot(&self) -> LogStreamSnapshot {
        self.stream.read().clone()
    }

    /// Returns whether the background monitor task has terminated unexpectedly.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for LogMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Formats buffered Mihomo log entries as a stable, line-oriented text file.
#[must_use]
pub fn format_log_entries(entries: &[LogEntry]) -> String {
    let mut output = String::new();
    for entry in entries {
        use std::fmt::Write as _;

        let level = if entry.level.trim().is_empty() {
            "INFO"
        } else {
            entry.level.trim()
        };
        let source = match entry.time_source {
            LogTimeSource::Core => "core",
            LogTimeSource::LocalReceive => "local-receive",
        };
        let time = entry
            .core_time
            .as_deref()
            .filter(|_| entry.time_source == LogTimeSource::Core)
            .map_or_else(|| entry.timestamp_ms.to_string(), str::to_owned);
        let fields = if entry.fields.is_null() {
            String::new()
        } else {
            format!(
                " {}",
                serde_json::to_string(&entry.fields).unwrap_or_else(|_| "{}".into())
            )
        };
        let _ = writeln!(
            output,
            "[{time}] [{source}] {:<7} {}{fields}",
            level.to_ascii_uppercase(),
            entry.payload,
        );
    }
    output
}

/// Atomically writes a prepared log export, privately on Unix.
/// Formatting and support-safe redaction remain the caller's explicit choice.
///
/// # Errors
/// Returns I/O failures for an unavailable parent, private-file creation or replacement.
pub fn write_log_export(path: &std::path::Path, payload: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let metadata = std::fs::metadata(parent)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "log export parent is not a directory",
            ));
        }
    }
    let _write_lease = crate::platform::data_coordinator::DataWriteAccess::new(path).acquire()?;
    private_io::replace(path, payload.as_bytes())
}

/// Formats a support-safe copy with messages and arbitrary fields omitted.
///
/// Log payloads routinely contain destinations, credentials and local paths,
/// so the safe representation retains only timing source and severity.
#[must_use]
pub fn format_log_entries_support_safe(entries: &[LogEntry]) -> String {
    let mut output = String::new();
    for entry in entries {
        use std::fmt::Write as _;

        let source = match entry.time_source {
            LogTimeSource::Core => "core",
            LogTimeSource::LocalReceive => "local-receive",
        };
        let level = if entry.level.trim().is_empty() {
            "INFO"
        } else {
            entry.level.trim()
        };
        let _ = writeln!(
            output,
            "[{}] [{source}] {} [message omitted]",
            entry.timestamp_ms,
            level.to_ascii_uppercase()
        );
    }
    output
}

struct LogMonitorState {
    entries: Arc<RwLock<LogBuffer>>,
    stream: Arc<RwLock<LogStreamSnapshot>>,
    revision: Arc<AtomicU64>,
    expected_generation: Arc<AtomicU64>,
    file_sender: file::LogFileSender,
}

async fn run_monitor(
    binding: Arc<ControllerBinding>,
    mut level: watch::Receiver<MihomoLogLevel>,
    mut generation_updates: watch::Receiver<u64>,
    state: LogMonitorState,
) {
    let mut backoff = crate::mihomo::websocket::ReconnectBackoff::default();
    let mut transport_updates = binding.subscribe();
    loop {
        let transport_generation = transport_updates.borrow_and_update().generation;
        let requested_level = *level.borrow_and_update();
        let generation = *generation_updates.borrow_and_update();
        let mut level_changed = false;
        let mut generation_changed = false;
        let connection = tokio::select! {
            changed = transport_updates.changed() => { if changed.is_err() { return; } continue; }
            changed = generation_updates.changed() => { if changed.is_err() { return; } continue; }
            changed = level.changed() => { if changed.is_err() { return; } continue; }
            connection = connect_log_stream(&binding, requested_level) => connection,
        };
        match connection {
            Ok(mut socket) => {
                if update_log_connection(
                    &state.stream,
                    true,
                    None,
                    generation,
                    &state.expected_generation,
                ) {
                    state.revision.fetch_add(1, Ordering::AcqRel);
                }
                loop {
                    let message = tokio::select! {
                        changed = transport_updates.changed() => {
                            if changed.is_err() { return; }
                            generation_changed = true;
                            break;
                        }
                        changed = level.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            level_changed = true;
                            break;
                        }
                        changed = generation_updates.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            generation_changed = true;
                            break;
                        }
                        message = socket.next() => message,
                    };
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        Ok(message)
                            if (message.is_text() || message.is_binary())
                                && binding.is_current(transport_generation) =>
                        {
                            match parse_log_frame(&message.into_data(), now_ms()) {
                                Ok(entry) => {
                                    if !log_level_accepts(requested_level, &entry.level) {
                                        continue;
                                    }
                                    backoff.reset();
                                    accept_log_entry(
                                        &state.entries,
                                        &state.stream,
                                        &state.revision,
                                        &state.file_sender,
                                        entry,
                                        generation,
                                        &state.expected_generation,
                                    );
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "received malformed Mihomo log frame");
                                    push_monitor_error_for_generation(
                                        &state.entries,
                                        &state.stream,
                                        &state.revision,
                                        &state.file_sender,
                                        format!("收到无法解析的 Mihomo 日志帧：{error}"),
                                        generation,
                                        &state.expected_generation,
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            push_monitor_error_for_generation(
                                &state.entries,
                                &state.stream,
                                &state.revision,
                                &state.file_sender,
                                format!("日志流读取失败：{error}"),
                                generation,
                                &state.expected_generation,
                            );
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                push_monitor_error_for_generation(
                    &state.entries,
                    &state.stream,
                    &state.revision,
                    &state.file_sender,
                    format!("日志流连接失败：{error}"),
                    generation,
                    &state.expected_generation,
                );
            }
        }
        if update_log_connection(
            &state.stream,
            false,
            None,
            generation,
            &state.expected_generation,
        ) {
            state.revision.fetch_add(1, Ordering::AcqRel);
        }
        if level_changed || generation_changed {
            backoff.reset();
            continue;
        }
        tokio::select! {
            changed = transport_updates.changed() => {
                if changed.is_err() { return; }
                backoff.reset();
            }
            changed = level.changed() => {
                if changed.is_err() {
                    return;
                }
                backoff.reset();
            }
            changed = generation_updates.changed() => {
                if changed.is_err() {
                    return;
                }
                backoff.reset();
            }
            () = tokio::time::sleep(backoff.next_delay()) => {}
        }
    }
}

fn push_prepared_bounded(entries: &RwLock<LogBuffer>, entry: BufferedLogEntry) -> BufferedLogEntry {
    let mut entries = entries.write();
    let mut retired = Vec::new();
    if entry.entry.level == "error"
        && entries.entries.back().is_some_and(|previous| {
            previous.entry.level == "error" && previous.entry.payload == entry.entry.payload
        })
        && let Some(previous) = entries.entries.pop_back()
    {
        entries.bytes -= previous.bytes;
        retired.push(previous);
    }
    while entries.entries.len() >= MAX_LOG_ENTRIES
        || entries.bytes.saturating_add(entry.bytes) > MAX_LOG_BUFFER_BYTES
    {
        if let Some(previous) = entries.entries.pop_front() {
            entries.bytes -= previous.bytes;
            retired.push(previous);
        } else {
            break;
        }
    }
    entries.bytes += entry.bytes;
    entries.entries.push_back(entry.clone());
    drop(entries);
    drop(retired);
    entry
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

async fn connect_log_stream(
    binding: &Arc<ControllerBinding>,
    level: MihomoLogLevel,
) -> Result<ControllerStream, String> {
    let snapshot = binding.snapshot();
    let structured = binding
        .connect_pinned(
            &snapshot,
            "/logs",
            &[("level", level.api_value()), ("format", "structured")],
            "连接 Mihomo 结构化日志流超时",
        )
        .await;
    match structured {
        Ok(socket) => Ok(socket),
        Err(error) if snapshot.is_service() => Err(error),
        Err(structured_error) => binding
            .connect_pinned(
                &snapshot,
                "/logs",
                &[("level", level.api_value())],
                "连接 Mihomo 日志流超时",
            )
            .await
            .map_err(|plain_error| {
                format!("结构化日志不可用：{structured_error}；plain 回退失败：{plain_error}")
            }),
    }
}

fn log_level_accepts(level: MihomoLogLevel, received: &str) -> bool {
    let rank = |level| match level {
        MihomoLogLevel::Silent => 0,
        MihomoLogLevel::Error => 1,
        MihomoLogLevel::Warning => 2,
        MihomoLogLevel::Info => 3,
        MihomoLogLevel::Debug => 4,
    };
    MihomoLogLevel::from_api(received)
        .is_some_and(|received| received != MihomoLogLevel::Silent && rank(received) <= rank(level))
}

fn parse_log_frame(data: &[u8], received_at_ms: u64) -> Result<LogEntry, serde_json::Error> {
    let mut entry = serde_json::from_slice::<LogEntry>(data)?;
    entry.received_at_ms = received_at_ms;
    let core_timestamp = entry
        .core_time
        .as_deref()
        .and_then(|time| parse_core_timestamp(time, received_at_ms));
    match core_timestamp {
        Some(timestamp_ms) => {
            entry.timestamp_ms = timestamp_ms;
            entry.time_source = LogTimeSource::Core;
        }
        None => {
            entry.timestamp_ms = received_at_ms;
            entry.time_source = LogTimeSource::LocalReceive;
        }
    }
    Ok(entry)
}

fn parse_core_timestamp(time: &str, received_at_ms: u64) -> Option<u64> {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(time) {
        return u64::try_from(time.timestamp_millis()).ok();
    }

    // Mihomo's documented structured stream emits only local `HH:mm:ss`.
    // Anchor it to the closest local date so a frame crossing midnight does
    // not appear almost 24 hours away from its receive time.
    let time = NaiveTime::parse_from_str(time, "%H:%M:%S%.f").ok()?;
    let received_at_ms = i64::try_from(received_at_ms).ok()?;
    let received = chrono::DateTime::from_timestamp_millis(received_at_ms)?.with_timezone(&Local);
    [
        received.date_naive().checked_sub_days(Days::new(1)),
        Some(received.date_naive()),
        received.date_naive().checked_add_days(Days::new(1)),
    ]
    .into_iter()
    .flatten()
    .filter_map(|date| Local.from_local_datetime(&date.and_time(time)).earliest())
    .min_by_key(|candidate| candidate.timestamp_millis().abs_diff(received_at_ms))
    .and_then(|candidate| u64::try_from(candidate.timestamp_millis()).ok())
}

#[cfg(test)]
mod tests;
