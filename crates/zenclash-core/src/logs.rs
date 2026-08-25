use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{websocket::connect_stream, MihomoEndpoint};

const MAX_LOG_ENTRIES: usize = 2_000;

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
    task: JoinHandle<()>,
}

impl LogMonitor {
    /// Starts a log monitor at the requested Mihomo severity level.
    #[must_use]
    pub fn start(runtime: &Handle, endpoint: MihomoEndpoint, level: &str) -> Arc<Self> {
        let entries = Arc::new(RwLock::new(VecDeque::new()));
        let connected = Arc::new(RwLock::new(false));
        let task = runtime.spawn(run_monitor(
            endpoint,
            level.to_owned(),
            entries.clone(),
            connected.clone(),
        ));
        Arc::new(Self {
            entries,
            connected,
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

async fn run_monitor(
    endpoint: MihomoEndpoint,
    level: String,
    entries: Arc<RwLock<VecDeque<LogEntry>>>,
    connected: Arc<RwLock<bool>>,
) {
    loop {
        match connect_stream(
            &endpoint,
            "/logs",
            &[("level", level.as_str())],
            "连接 Mihomo 日志流超时",
        )
        .await
        {
            Ok(mut socket) => {
                *connected.write() = true;
                while let Some(message) = socket.next().await {
                    match message {
                        Ok(message) if message.is_text() || message.is_binary() => {
                            match serde_json::from_slice::<LogEntry>(&message.into_data()) {
                                Ok(mut entry) => {
                                    entry.timestamp_ms = now_ms();
                                    push_bounded(&entries, entry);
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "received malformed Mihomo log frame");
                                    push_monitor_error(
                                        &entries,
                                        "收到无法解析的 Mihomo 日志帧".into(),
                                    );
                                }
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(error) => {
                            push_monitor_error(&entries, format!("日志流读取失败：{error}"));
                            break;
                        }
                    }
                }
            }
            Err(error) => push_monitor_error(&entries, format!("日志流连接失败：{error}")),
        }
        *connected.write() = false;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn push_monitor_error(entries: &RwLock<VecDeque<LogEntry>>, payload: String) {
    push_bounded(
        entries,
        LogEntry {
            level: "error".into(),
            payload,
            timestamp_ms: now_ms(),
        },
    );
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
}
