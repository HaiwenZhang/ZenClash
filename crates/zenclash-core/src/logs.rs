use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
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
    #[serde(default, rename = "type")]
    pub level: String,
    /// Human-readable Mihomo log payload.
    #[serde(default)]
    pub payload: String,
    /// Local receive time as Unix milliseconds.
    #[serde(default)]
    pub timestamp_ms: u64,
}

/// Maintains a bounded, reconnecting Mihomo `/logs` stream.
pub struct LogMonitor {
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    connected: Arc<RwLock<bool>>,
    level: watch::Sender<MihomoLogLevel>,
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
        let connected = Arc::new(RwLock::new(false));
        let file = file::LogFileWorker::start();
        let (level_sender, level_receiver) = watch::channel(level);
        let task = runtime.spawn(run_monitor(
            endpoint,
            level_receiver,
            entries.clone(),
            connected.clone(),
            file.sender(),
        ));
        Arc::new(Self {
            entries,
            connected,
            level: level_sender,
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
        self.entries.write().clear();
    }

    /// Changes the stream threshold and reconnects only when it actually differs.
    /// Debug requests use the safe info threshold for the real-time stream.
    pub fn set_level(&self, level: MihomoLogLevel) {
        let level = level.realtime_stream_level();
        if *self.level.borrow() != level {
            self.level.send_replace(level);
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
        *self.connected.read()
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
        let _ = writeln!(
            output,
            "[{}] {:<7} {}",
            entry.timestamp_ms,
            level.to_ascii_uppercase(),
            entry.payload
        );
    }
    output
}

async fn run_monitor(
    endpoint: MihomoEndpoint,
    mut level: watch::Receiver<MihomoLogLevel>,
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    connected: Arc<RwLock<bool>>,
    file_sender: file::LogFileSender,
) {
    loop {
        let requested_level = *level.borrow();
        let mut level_changed = false;
        match connect_stream(
            &endpoint,
            "/logs",
            &[("level", requested_level.api_value())],
            "连接 Mihomo 日志流超时",
        )
        .await
        {
            Ok(mut socket) => {
                *connected.write() = true;
                loop {
                    let message = tokio::select! {
                        changed = level.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            level_changed = true;
                            break;
                        }
                        message = socket.next() => message,
                    };
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        Ok(message) if message.is_text() || message.is_binary() => {
                            match serde_json::from_slice::<LogEntry>(&message.into_data()) {
                                Ok(mut entry) => {
                                    entry.timestamp_ms = now_ms();
                                    push_bounded(&entries, entry.clone());
                                    file_sender.append(entry);
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "received malformed Mihomo log frame");
                                    push_monitor_error(
                                        &entries,
                                        &file_sender,
                                        "收到无法解析的 Mihomo 日志帧".into(),
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            push_monitor_error(
                                &entries,
                                &file_sender,
                                format!("日志流读取失败：{error}"),
                            );
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                push_monitor_error(&entries, &file_sender, format!("日志流连接失败：{error}"));
            }
        }
        *connected.write() = false;
        if level_changed {
            continue;
        }
        tokio::select! {
            changed = level.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

fn push_monitor_error(
    entries: &RwLock<VecDeque<LogEntry>>,
    file_sender: &file::LogFileSender,
    payload: String,
) {
    let entry = LogEntry {
        level: "error".into(),
        payload,
        timestamp_ms: now_ms(),
    };
    push_bounded(entries, entry.clone());
    file_sender.append(entry);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_log_frame() {
        let entry: LogEntry = serde_json::from_str(
            r#"{"type":"info","payload":"[TCP] connected to example.com:443"}"#,
        )
        .unwrap();
        assert_eq!(entry.level, "info");
        assert!(entry.payload.contains("example.com"));
    }

    #[test]
    fn text_export_preserves_order_and_severity() {
        let entries = [
            LogEntry {
                level: "info".into(),
                payload: "first".into(),
                timestamp_ms: 10,
            },
            LogEntry {
                level: "warn".into(),
                payload: "second".into(),
                timestamp_ms: 20,
            },
        ];

        assert_eq!(
            format_log_entries(&entries),
            "[10] INFO    first\n[20] WARN    second\n"
        );
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
            },
        );

        push_bounded(
            &entries,
            LogEntry {
                level: "error".into(),
                payload: "offline".into(),
                timestamp_ms: 2,
            },
        );

        assert_eq!(
            entries.read().iter().cloned().collect::<Vec<_>>(),
            vec![LogEntry {
                level: "error".into(),
                payload: "offline".into(),
                timestamp_ms: 2,
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
