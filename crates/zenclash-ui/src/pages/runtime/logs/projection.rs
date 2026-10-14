//! Cached rows, filtered snapshots and cancellable background log projection.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Default)]
pub(super) struct LogPresentation {
    pub(super) revision: Option<u64>,
    pub(super) query: String,
    pub(super) entries: Vec<Arc<zenclash_core::LogEntry>>,
    pub(super) rows: Vec<LogRow>,
    pub(super) matches: Vec<usize>,
    pub(super) level_counts: Vec<(String, u64)>,
    pub(super) level_filter: Option<String>,
    pub(super) oldest_first: bool,
}

#[derive(Clone)]
pub(super) struct LogRow {
    pub(super) level: gpui_kit::SharedString,
    pub(super) payload: gpui_kit::SharedString,
    pub(super) time: gpui_kit::SharedString,
    pub(super) time_source: LogTimeSource,
}

impl From<&zenclash_core::LogEntry> for LogRow {
    fn from(entry: &zenclash_core::LogEntry) -> Self {
        Self {
            level: normalized_level(&entry.level).into(),
            payload: entry.payload.clone().into(),
            time: format_log_time(entry).into(),
            time_source: entry.time_source,
        }
    }
}

pub(super) fn format_log_time(entry: &zenclash_core::LogEntry) -> String {
    if let Some(time) = &entry.core_time {
        if let Ok(time) = chrono::DateTime::parse_from_rfc3339(time) {
            return time
                .with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string();
        }
        if chrono::NaiveTime::parse_from_str(time, "%H:%M:%S").is_ok() {
            return time.clone();
        }
    }
    i64::try_from(entry.timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

impl LogPresentation {
    pub(super) fn refresh(
        &mut self,
        revision: u64,
        query: String,
        snapshot: impl FnOnce() -> Vec<Arc<zenclash_core::LogEntry>>,
    ) {
        let changed = self.revision != Some(revision);
        if changed {
            self.entries = snapshot();
            self.rows = self
                .entries
                .iter()
                .map(|entry| LogRow::from(entry.as_ref()))
                .collect();
            self.revision = Some(revision);
            let mut counts = std::collections::BTreeMap::<String, u64>::new();
            for entry in &self.entries {
                *counts.entry(normalized_level(&entry.level)).or_default() += 1;
            }
            self.level_counts = counts.into_iter().collect();
        }
        if changed || self.query != query {
            self.query = query;
            self.matches = self
                .entries
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, entry)| log_matches(entry, &self.query))
                .filter(|(_, entry)| {
                    self.level_filter
                        .as_ref()
                        .is_none_or(|level| normalized_level(&entry.level) == *level)
                })
                .map(|(index, _)| index)
                .collect();
            if self.oldest_first {
                self.matches.reverse();
            }
        }
    }

    pub(super) fn set_order(&mut self, oldest_first: bool) {
        if self.oldest_first != oldest_first {
            self.matches.reverse();
            self.oldest_first = oldest_first;
        }
    }

    pub(super) fn set_level_filter(&mut self, level: Option<String>) {
        self.level_filter = level;
        self.matches = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| log_matches(entry, &self.query))
            .filter(|(_, entry)| {
                self.level_filter
                    .as_ref()
                    .is_none_or(|level| normalized_level(&entry.level) == *level)
            })
            .map(|(index, _)| index)
            .collect();
        if self.oldest_first {
            self.matches.reverse();
        }
    }
}

pub(super) struct LogSnapshot {
    pub(super) revision: u64,
    pub(super) entries: Option<Vec<Arc<zenclash_core::LogEntry>>>,
    pub(super) connected: bool,
    pub(super) level: MihomoLogLevel,
    pub(super) persistence: zenclash_core::LogPersistenceStatus,
    pub(super) pending_entries: usize,
}

pub(super) struct PreparedLogView {
    pub(super) presentation: Arc<LogPresentation>,
    pub(super) connected: bool,
    pub(super) level: MihomoLogLevel,
    pub(super) persistence: zenclash_core::LogPersistenceStatus,
    pub(super) pending_entries: usize,
}

#[derive(Default)]
pub(super) struct LogProjectionWorker {
    pub(super) generation: Arc<AtomicU64>,
    pub(super) gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) task: super::super::page_observation::PageReadTask,
}

impl LogProjectionWorker {
    pub(super) fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    #[cfg(test)]
    pub(super) fn start(
        &mut self,
        runtime: &tokio::runtime::Handle,
        previous: Arc<LogPresentation>,
        query: String,
        source: impl FnOnce() -> LogSnapshot + Send + 'static,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<PreparedLogView>, String>>,
    ) {
        self.start_filtered(runtime, previous, query, None, false, source)
    }

    pub(super) fn start_filtered(
        &mut self,
        runtime: &tokio::runtime::Handle,
        previous: Arc<LogPresentation>,
        query: String,
        level_filter: Option<String>,
        oldest_first: bool,
        source: impl FnOnce() -> LogSnapshot + Send + 'static,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<PreparedLogView>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();
        let task = runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let snapshot = source();
                if current.load(Ordering::Acquire) != generation {
                    return None;
                }
                let presentation = if previous.revision == Some(snapshot.revision)
                    && previous.query == query
                    && previous.level_filter == level_filter
                    && previous.oldest_first == oldest_first
                {
                    previous
                } else {
                    let mut presentation = previous.as_ref().clone();
                    presentation.refresh(snapshot.revision, query, || {
                        snapshot.entries.unwrap_or_default()
                    });
                    if presentation.level_filter != level_filter {
                        presentation.set_level_filter(level_filter);
                    }
                    presentation.set_order(oldest_first);
                    Arc::new(presentation)
                };
                (current.load(Ordering::Acquire) == generation).then_some(PreparedLogView {
                    presentation,
                    connected: snapshot.connected,
                    level: snapshot.level,
                    persistence: snapshot.persistence,
                    pending_entries: snapshot.pending_entries,
                })
            })
            .await
            .map_err(|error| error.to_string())
        });
        self.task.replace(&task);
        (generation, task)
    }
}

impl Drop for LogProjectionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}
