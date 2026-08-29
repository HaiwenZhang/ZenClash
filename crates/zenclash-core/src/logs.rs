use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{Days, Local, NaiveTime, TimeZone};
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{MihomoEndpoint, websocket::connect_stream};

mod file;

pub use file::{LogPersistenceError, LogPersistenceResult, LogPersistenceStatus};

const MAX_LOG_ENTRIES: usize = 500;

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

    const fn realtime_stream_level(self) -> Self {
        // meow-rs broadcasts its WebSocket protocol DEBUG events through the
        // same `/logs` stream. Subscribing at DEBUG therefore records each
        // outgoing frame as another outgoing frame. The native UI intentionally
        // keeps operational logs while leaving verbose diagnostics to the core's
        // own stdout/file sink.
        match self {
            Self::Debug => Self::Info,
            level => level,
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
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
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
    /// Debug is normalized to info for the real-time WebSocket because some
    /// compatible cores emit their own frame-writing diagnostics into `/logs`.
    #[must_use]
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint, level: MihomoLogLevel) -> Arc<Self> {
        let level = level.realtime_stream_level();
        let entries = Arc::new(RwLock::new(VecDeque::new()));
        let stream = Arc::new(RwLock::new(LogStreamSnapshot::default()));
        let revision = Arc::new(AtomicU64::new(0));
        let file = file::LogFileWorker::start();
        let (level_sender, level_receiver) = watch::channel(level);
        let expected_generation = Arc::new(AtomicU64::new(0));
        let (generation, generation_receiver) = watch::channel(0);
        let task = runtime.spawn(run_monitor(
            endpoint,
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
        self.entries.read().iter().cloned().collect()
    }

    /// Removes all currently buffered log entries.
    pub fn clear(&self) {
        let mut entries = self.entries.write();
        if !entries.is_empty() {
            entries.clear();
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Returns a monotonic revision for visible entries and stream connection state.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Changes the stream threshold and reconnects only when it actually differs.
    /// Debug requests use the safe info threshold for the real-time stream.
    pub fn set_level(&self, level: MihomoLogLevel) {
        let level = level.realtime_stream_level();
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

    /// Changes the accepted core generation and reconnects the WebSocket.
    ///
    /// Frames already queued by the older socket are rejected before they can
    /// enter the visible log buffer.
    pub fn synchronize_generation(&self, generation: u64) {
        if self.expected_generation.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        self.stream.write().connected = false;
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.generation.send_replace(generation);
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
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    stream: Arc<RwLock<LogStreamSnapshot>>,
    revision: Arc<AtomicU64>,
    expected_generation: Arc<AtomicU64>,
    file_sender: file::LogFileSender,
}

async fn run_monitor(
    endpoint: MihomoEndpoint,
    mut level: watch::Receiver<MihomoLogLevel>,
    mut generation_updates: watch::Receiver<u64>,
    state: LogMonitorState,
) {
    loop {
        let requested_level = *level.borrow();
        let generation = *generation_updates.borrow_and_update();
        let mut level_changed = false;
        let mut generation_changed = false;
        match connect_log_stream(&endpoint, requested_level).await {
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
                        Ok(message) if message.is_text() || message.is_binary() => {
                            match parse_log_frame(&message.into_data(), now_ms()) {
                                Ok(entry) => {
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
            continue;
        }
        tokio::select! {
            changed = level.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = generation_updates.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

fn update_log_connection(
    stream: &RwLock<LogStreamSnapshot>,
    connected: bool,
    error: Option<String>,
    generation: u64,
    expected_generation: &AtomicU64,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let mut stream = stream.write();
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

fn accept_log_entry(
    entries: &RwLock<VecDeque<LogEntry>>,
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
    {
        let mut stream = stream.write();
        if stream.generation != generation {
            return false;
        }
        stream.connected = true;
        stream.updated_at_ms = if entry.received_at_ms == 0 {
            entry.timestamp_ms
        } else {
            entry.received_at_ms
        };
        stream.last_error = None;
        stream.format = if entry.core_time.is_some() || !entry.fields.is_null() {
            LogStreamFormat::Structured
        } else {
            LogStreamFormat::Plain
        };
    }
    push_bounded(entries, entry.clone());
    revision.fetch_add(1, Ordering::AcqRel);
    file_sender.append(entry);
    true
}

fn push_monitor_error_for_generation(
    entries: &RwLock<VecDeque<LogEntry>>,
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
    let timestamp_ms = now_ms();
    {
        let mut stream = stream.write();
        if stream.generation != generation {
            *stream = LogStreamSnapshot {
                generation,
                ..LogStreamSnapshot::default()
            };
        }
        stream.last_error = Some(payload.clone());
    }
    let entry = LogEntry {
        level: "error".into(),
        payload,
        timestamp_ms,
        ..LogEntry::default()
    };
    push_bounded(entries, entry.clone());
    revision.fetch_add(1, Ordering::AcqRel);
    file_sender.append(entry);
    true
}

fn push_bounded(entries: &RwLock<VecDeque<LogEntry>>, entry: LogEntry) {
    let mut entries = entries.write();
    if entry.level == "error"
        && entries
            .back()
            .is_some_and(|previous| previous.level == "error" && previous.payload == entry.payload)
    {
        if let Some(previous) = entries.back_mut() {
            previous.timestamp_ms = entry.timestamp_ms;
            previous.received_at_ms = entry.received_at_ms;
        }
        return;
    }
    if entries.len() >= MAX_LOG_ENTRIES {
        entries.pop_front();
    }
    entries.push_back(entry);
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
    endpoint: &MihomoEndpoint,
    level: MihomoLogLevel,
) -> Result<crate::websocket::MihomoSocket, String> {
    let structured = connect_stream(
        endpoint,
        "/logs",
        &[("level", level.api_value()), ("format", "structured")],
        "连接 Mihomo 结构化日志流超时",
    )
    .await;
    match structured {
        Ok(socket) => Ok(socket),
        Err(structured_error) => connect_stream(
            endpoint,
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
mod tests {
    use super::*;

    #[test]
    fn parses_real_log_frame() {
        let entry = parse_log_frame(
            br#"{"type":"info","payload":"[TCP] connected to example.com:443"}"#,
            42,
        )
        .unwrap();
        assert_eq!(entry.level, "info");
        assert!(entry.payload.contains("example.com"));
        assert_eq!(entry.timestamp_ms, 42);
        assert_eq!(entry.received_at_ms, 42);
        assert_eq!(entry.time_source, LogTimeSource::LocalReceive);
    }

    #[test]
    fn structured_log_preserves_core_time_level_message_and_fields() {
        let entry = parse_log_frame(
            br#"{"time":"2026-08-27T12:34:56.789Z","level":"warning","message":"dial failed","fields":{"network":"tcp","attempt":2}}"#,
            1,
        )
        .unwrap();

        assert_eq!(entry.level, "warning");
        assert_eq!(entry.payload, "dial failed");
        assert_eq!(entry.core_time.as_deref(), Some("2026-08-27T12:34:56.789Z"));
        assert_eq!(entry.fields["network"], "tcp");
        assert_eq!(entry.fields["attempt"], 2);
        assert_eq!(entry.timestamp_ms, 1_787_834_096_789);
        assert_eq!(entry.time_source, LogTimeSource::Core);
        assert_eq!(
            entry.filtered_fields(&["network"]),
            BTreeMap::from([("network".into(), Value::String("tcp".into()))])
        );
    }

    #[test]
    fn structured_log_preserves_array_shaped_mihomo_fields() {
        let entry = parse_log_frame(
            br#"{"time":"2026-08-27T12:34:56Z","level":"info","message":"ready","fields":["listener","mixed"]}"#,
            1,
        )
        .unwrap();

        assert_eq!(entry.fields, serde_json::json!(["listener", "mixed"]));
        assert_eq!(entry.time_source, LogTimeSource::Core);
        assert!(entry.filtered_fields(&["listener"]).is_empty());
    }

    #[test]
    fn structured_time_only_uses_the_closest_local_date() {
        let received = Local
            .with_ymd_and_hms(2026, 8, 28, 0, 0, 1)
            .single()
            .expect("unambiguous local test time");
        let entry = parse_log_frame(
            br#"{"time":"23:59:59","level":"info","message":"ready","fields":[]}"#,
            u64::try_from(received.timestamp_millis()).unwrap(),
        )
        .unwrap();
        let expected = Local
            .with_ymd_and_hms(2026, 8, 27, 23, 59, 59)
            .single()
            .expect("unambiguous local test time");

        assert_eq!(
            entry.timestamp_ms,
            u64::try_from(expected.timestamp_millis()).unwrap()
        );
        assert_eq!(entry.time_source, LogTimeSource::Core);
    }

    #[test]
    fn text_export_preserves_order_and_severity() {
        let entries = [
            LogEntry {
                level: "info".into(),
                payload: "first".into(),
                timestamp_ms: 10,
                ..LogEntry::default()
            },
            LogEntry {
                level: "warn".into(),
                payload: "second".into(),
                timestamp_ms: 20,
                ..LogEntry::default()
            },
        ];

        assert_eq!(
            format_log_entries(&entries),
            "[10] [local-receive] INFO    first\n[20] [local-receive] WARN    second\n"
        );
    }

    #[test]
    fn support_safe_log_copy_omits_messages_fields_and_core_time() {
        let entry = LogEntry {
            level: "info".into(),
            payload: "Bearer secret https://example.com/?token=raw /Users/alice/config".into(),
            core_time: Some("private-core-time".into()),
            fields: serde_json::json!({"password": "secret"}),
            timestamp_ms: 10,
            received_at_ms: 10,
            time_source: LogTimeSource::Core,
        };

        let safe = format_log_entries_support_safe(&[entry]);

        assert_eq!(safe, "[10] [core] INFO [message omitted]\n");
    }

    #[test]
    fn repeated_connection_error_updates_timestamp_without_flooding_log() {
        let entries = RwLock::new(VecDeque::new());
        push_bounded(
            &entries,
            LogEntry {
                level: "error".into(),
                payload: "offline".into(),
                timestamp_ms: 1,
                ..LogEntry::default()
            },
        );

        push_bounded(
            &entries,
            LogEntry {
                level: "error".into(),
                payload: "offline".into(),
                timestamp_ms: 2,
                ..LogEntry::default()
            },
        );

        assert_eq!(
            entries.read().iter().cloned().collect::<Vec<_>>(),
            vec![LogEntry {
                level: "error".into(),
                payload: "offline".into(),
                timestamp_ms: 2,
                ..LogEntry::default()
            }]
        );
    }

    #[test]
    fn bounded_log_buffer_discards_the_oldest_entry() {
        let entries = RwLock::new(
            (0..MAX_LOG_ENTRIES)
                .map(|index| LogEntry {
                    payload: index.to_string(),
                    ..LogEntry::default()
                })
                .collect(),
        );

        push_bounded(
            &entries,
            LogEntry {
                payload: "latest".into(),
                ..LogEntry::default()
            },
        );

        let entries = entries.read();
        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(
            entries.front().map(|entry| entry.payload.as_str()),
            Some("1")
        );
        assert_eq!(
            entries.back().map(|entry| entry.payload.as_str()),
            Some("latest")
        );
        drop(entries);
    }

    #[test]
    fn log_levels_parse_mihomo_values_case_insensitively() {
        assert_eq!(
            MihomoLogLevel::from_api(" WARNING "),
            Some(MihomoLogLevel::Warning)
        );
        assert_eq!(MihomoLogLevel::from_api("trace"), None);
    }

    #[test]
    fn realtime_stream_caps_debug_to_operational_logs() {
        assert_eq!(
            MihomoLogLevel::Debug.realtime_stream_level(),
            MihomoLogLevel::Info
        );
        assert_eq!(
            MihomoLogLevel::Warning.realtime_stream_level(),
            MihomoLogLevel::Warning
        );
    }

    #[test]
    fn late_log_frame_from_an_old_generation_is_rejected() {
        let entries = RwLock::new(VecDeque::new());
        let stream = RwLock::new(LogStreamSnapshot {
            generation: 2,
            connected: true,
            ..LogStreamSnapshot::default()
        });
        let expected_generation = AtomicU64::new(2);
        let revision = AtomicU64::new(0);
        let file = file::LogFileWorker::start();

        let accepted = accept_log_entry(
            &entries,
            &stream,
            &revision,
            &file.sender(),
            LogEntry {
                level: "info".into(),
                payload: "old session".into(),
                timestamp_ms: 10,
                ..LogEntry::default()
            },
            1,
            &expected_generation,
        );

        assert!(!accepted);
        assert!(entries.read().is_empty());
        assert_eq!(stream.read().generation, 2);
        assert_eq!(stream.read().updated_at_ms, 0);
        assert_eq!(revision.load(Ordering::Acquire), 0);
    }

    #[test]
    fn stream_interruption_preserves_last_entry_time_and_format() {
        let stream = RwLock::new(LogStreamSnapshot {
            generation: 2,
            connected: true,
            updated_at_ms: 123,
            format: LogStreamFormat::Structured,
            ..LogStreamSnapshot::default()
        });
        let expected_generation = AtomicU64::new(2);

        assert!(update_log_connection(
            &stream,
            false,
            None,
            2,
            &expected_generation
        ));

        assert!(!stream.read().connected);
        assert_eq!(stream.read().updated_at_ms, 123);
        assert_eq!(stream.read().format, LogStreamFormat::Structured);
    }

    #[tokio::test]
    async fn changing_log_level_updates_the_monitor_without_restarting_it() {
        let monitor = LogMonitor::start(
            &Handle::current(),
            MihomoEndpoint::default(),
            MihomoLogLevel::Info,
        );

        monitor.set_level(MihomoLogLevel::Warning);

        assert_eq!(monitor.level(), MihomoLogLevel::Warning);
    }
}
