use std::collections::HashMap;

use chrono::DateTime;

use super::TrafficHistoryEntry;
use crate::{Connection, ConnectionsSnapshot};

const MAX_LABEL_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    upload: u64,
    download: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ConnectionBaseline {
    counters: Counters,
    observed_epoch: u64,
}

/// Stateful converter from cumulative Mihomo connection counters to positive deltas.
#[derive(Debug)]
pub struct TrafficDeltaLogger {
    enabled_at_ms: u64,
    last_totals: Counters,
    observation_epoch: u64,
    connections: HashMap<String, ConnectionBaseline>,
}

impl TrafficDeltaLogger {
    /// Starts a fresh accounting run at the supplied Unix-millisecond time.
    #[must_use]
    pub fn new(enabled_at_ms: u64) -> Self {
        Self {
            enabled_at_ms,
            last_totals: Counters::default(),
            observation_epoch: 0,
            connections: HashMap::new(),
        }
    }

    /// Clears connection baselines when logging is disabled and re-enabled.
    pub fn reset(&mut self, enabled_at_ms: u64) {
        self.enabled_at_ms = enabled_at_ms;
        self.last_totals = Counters::default();
        self.observation_epoch = 0;
        self.connections.clear();
    }

    /// Observes a real `/connections` snapshot and returns non-zero byte deltas.
    ///
    /// Connections that predate this logger run establish a baseline without
    /// attributing their earlier bytes. A Mihomo restart is detected when its
    /// aggregate counters decrease, which invalidates every connection baseline.
    #[must_use]
    pub fn observe(
        &mut self,
        snapshot: &ConnectionsSnapshot,
        now_ms: u64,
    ) -> Vec<TrafficHistoryEntry> {
        if snapshot.upload_total < self.last_totals.upload
            || snapshot.download_total < self.last_totals.download
        {
            self.connections.clear();
        }
        self.last_totals = Counters {
            upload: snapshot.upload_total,
            download: snapshot.download_total,
        };
        if snapshot.connections.is_empty() {
            self.connections.clear();
            return Vec::new();
        }

        self.observation_epoch = self.observation_epoch.wrapping_add(1);
        if self.observation_epoch == 0 {
            self.connections.clear();
            self.observation_epoch = 1;
        }
        let observation_epoch = self.observation_epoch;
        let mut entries = Vec::new();
        for connection in &snapshot.connections {
            if connection.id.is_empty() {
                continue;
            }
            let current = Counters {
                upload: connection.upload,
                download: connection.download,
            };
            let previous = if let Some(baseline) = self.connections.get_mut(&connection.id) {
                let previous = baseline.counters;
                baseline.counters = current;
                baseline.observed_epoch = observation_epoch;
                Some(previous)
            } else {
                self.connections.insert(
                    connection.id.clone(),
                    ConnectionBaseline {
                        counters: current,
                        observed_epoch: observation_epoch,
                    },
                );
                None
            };
            let log_initial = previous.is_none() && self.connection_started_in_run(connection);
            let upload = previous.map_or_else(
                || u64::from(log_initial) * current.upload,
                |last| current.upload.saturating_sub(last.upload),
            );
            let download = previous.map_or_else(
                || u64::from(log_initial) * current.download,
                |last| current.download.saturating_sub(last.download),
            );
            if upload == 0 && download == 0 {
                continue;
            }
            entries.push(history_entry(connection, now_ms, upload, download));
        }
        self.connections
            .retain(|_, baseline| baseline.observed_epoch == observation_epoch);
        entries
    }

    fn connection_started_in_run(&self, connection: &Connection) -> bool {
        DateTime::parse_from_rfc3339(&connection.start)
            .ok()
            .and_then(|timestamp| u64::try_from(timestamp.timestamp_millis()).ok())
            .is_some_and(|started_at| started_at >= self.enabled_at_ms)
    }
}

fn history_entry(
    connection: &Connection,
    timestamp_ms: u64,
    upload: u64,
    download: u64,
) -> TrafficHistoryEntry {
    let metadata = &connection.metadata;
    TrafficHistoryEntry {
        timestamp_ms,
        source_ip: bounded_label(&metadata.source_ip, "Inner"),
        host: bounded_label(
            if metadata.host.is_empty() {
                &metadata.destination_ip
            } else {
                &metadata.host
            },
            "Unknown",
        ),
        outbound: bounded_label(
            connection.chains.first().map_or("DIRECT", String::as_str),
            "DIRECT",
        ),
        process: bounded_label(&metadata.process, "Unknown"),
        upload,
        download,
    }
}

fn bounded_label(value: &str, fallback: &str) -> String {
    let value = value.trim();
    let value = if value.is_empty() { fallback } else { value };
    value.chars().take(MAX_LABEL_CHARS).collect()
}
