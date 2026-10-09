use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use zenclash_core::{TrafficDimension, TrafficHistoryQuery, TrafficHistoryStore};

const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum HistoryStatus {
    #[default]
    Loading,
    Ready,
    StoreUnavailable,
    Failed,
    Stale,
}
struct HistoryRank {
    label: Option<String>,
    bytes: u64,
}
struct HistorySummary {
    ranks: Vec<HistoryRank>,
}
fn read_history(
    store: &TrafficHistoryStore,
    end_ms: u64,
    epoch: &AtomicU64,
    expected: u64,
) -> Result<Option<HistorySummary>, String> {
    if epoch.load(Ordering::Acquire) != expected {
        return Ok(None);
    }
    let overview = store
        .overview(&TrafficHistoryQuery {
            dimension: TrafficDimension::Process,
            start_ms: end_ms.saturating_sub(DAY_MS - 1),
            end_ms,
            bucket_ms: 60 * 60 * 1_000,
        })
        .map_err(|error| error.to_string())?;
    if epoch.load(Ordering::Acquire) != expected {
        return Ok(None);
    }
    let mut ranks = overview
        .rankings
        .into_iter()
        .take(5)
        .map(|rank| HistoryRank {
            label: Some(rank.label),
            bytes: rank.total,
        })
        .collect::<Vec<_>>();
    let top = ranks
        .iter()
        .fold(0u64, |sum, rank| sum.saturating_add(rank.bytes));
    let other = overview.totals.total.saturating_sub(top);
    if other > 0 {
        ranks.push(HistoryRank {
            label: None,
            bytes: other,
        });
    }
    Ok(Some(HistorySummary { ranks }))
}
#[derive(Default)]
pub(super) struct HomeHistoryState {
    epoch: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::super::loader::PageReadTask,
    in_flight: bool,
    requested_at: Option<Instant>,
    source: Option<std::path::PathBuf>,
    status: HistoryStatus,
    summary: Option<HistorySummary>,
}
impl HomeHistoryState {
    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn prepare_design_validation(&mut self) {
        self.release();
        self.status = HistoryStatus::Ready;
        self.summary = Some(HistorySummary {
            ranks: ["browser.exe", "ChatGPT.exe", "terminal.exe", "explorer.exe"]
                .into_iter()
                .enumerate()
                .map(|(index, label)| HistoryRank {
                    label: Some(label.into()),
                    bytes: (4 - index) as u64 * 64 * 1024 * 1024,
                })
                .collect(),
        });
    }

    pub(super) fn release(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
        self.in_flight = false;
        self.requested_at = None;
        self.source = None;
        self.summary = None;
        self.status = HistoryStatus::Loading;
    }
    fn finish(&mut self, expected: u64, result: Result<Option<HistorySummary>, String>) -> bool {
        if self.epoch.load(Ordering::Acquire) != expected {
            return false;
        }
        self.in_flight = false;
        match result {
            Ok(Some(summary)) => {
                self.summary = Some(summary);
                self.status = HistoryStatus::Ready;
            }
            Ok(None) => {}
            Err(_) => {
                self.status = if self.summary.is_some() {
                    HistoryStatus::Stale
                } else {
                    HistoryStatus::Failed
                }
            }
        }
        true
    }
}
impl Drop for HomeHistoryState {
    fn drop(&mut self) {
        self.release();
    }
}
impl RuntimePage {
    pub(super) fn update_home_history(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Home {
            self.home.history.release();
            return;
        }
        let Some(store) = self.traffic_history_store.clone() else {
            if self.home.history.status != HistoryStatus::StoreUnavailable {
                self.home.history.release();
                self.home.history.status = HistoryStatus::StoreUnavailable;
                cx.notify();
            }
            return;
        };
        let source = store.path().to_owned();
        let state = &mut self.home.history;
        if state.source.as_ref() != Some(&source) {
            state.release();
            state.source = Some(source.clone());
        }
        if state.in_flight
            || state
                .requested_at
                .is_some_and(|time| time.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        state.in_flight = true;
        state.requested_at = Some(Instant::now());
        let expected = state.epoch.load(Ordering::Acquire);
        let epoch = state.epoch.clone();
        let gate = state.gate.clone();
        let generation = self.home.generation;
        let end_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| {
                u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
            });
        let task = self.runtime.spawn(async move {
            let permit = gate.lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                read_history(&store, end_ms, &epoch, expected)
            })
            .await
            .map_err(|error| error.to_string())?
        });
        state.task.replace(&task);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.page != Page::Home
                    || this.home.generation != generation
                    || this.core_session.generation() != generation
                    || this
                        .traffic_history_store
                        .as_ref()
                        .is_none_or(|store| store.path() != source)
                {
                    return;
                }
                if this.home.history.finish(expected, result) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::TrafficHistoryEntry;

    fn entry(timestamp_ms: u64, process: &str, bytes: u64) -> TrafficHistoryEntry {
        TrafficHistoryEntry {
            timestamp_ms,
            source_ip: "127.0.0.1".into(),
            host: "fixture.invalid".into(),
            outbound: "DIRECT".into(),
            process: process.into(),
            upload: bytes,
            download: 0,
        }
    }

    #[test]
    fn real_sqlite_history_keeps_exact_day_boundary_top_five_and_other_totals() {
        let directory = crate::pages::runtime::logs::tests::settings_test_directory("home-history");
        let store = TrafficHistoryStore::new(directory.join("history.sqlite3"));
        let end = 100_000_000;
        let start = end - DAY_MS + 1;
        let mut rows = (1..=7)
            .map(|index| entry(end, &format!("process-{index}"), index))
            .collect::<Vec<_>>();
        rows.push(entry(start, "boundary", 50));
        rows.push(entry(start - 1, "too-old", 999));
        rows.push(entry(end + 1, "future", 888));
        store.insert_and_cleanup(&rows, 0).unwrap();
        let summary = read_history(&store, end, &AtomicU64::new(0), 0)
            .unwrap()
            .unwrap();
        assert_eq!(summary.ranks.len(), 6);
        assert_eq!(summary.ranks[0].label.as_deref(), Some("boundary"));
        assert_eq!(summary.ranks[0].bytes, 50);
        assert_eq!(summary.ranks[1].label.as_deref(), Some("process-7"));
        assert_eq!(summary.ranks[5].label, None);
        assert_eq!(summary.ranks[5].bytes, 6);
        assert_eq!(summary.ranks.iter().map(|rank| rank.bytes).sum::<u64>(), 78);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn history_failure_retains_recorded_summary_and_cancel_rejects_completion() {
        let mut state = HomeHistoryState::default();
        assert!(state.finish(
            0,
            Ok(Some(HistorySummary {
                ranks: vec![HistoryRank {
                    label: Some("saved".into()),
                    bytes: 12
                }]
            }))
        ));
        assert!(state.finish(0, Err("database unavailable".into())));
        assert!(state.status == HistoryStatus::Stale);
        assert_eq!(state.summary.as_ref().unwrap().ranks[0].bytes, 12);
        assert!(!state.in_flight);
        state.release();
        assert!(!state.finish(0, Ok(Some(HistorySummary { ranks: Vec::new() }))));
        assert!(state.summary.is_none());
        assert!(state.finish(1, Err("database unavailable".into())));
        assert!(state.status == HistoryStatus::Failed);
    }

    #[test]
    fn cancelled_history_query_does_not_open_store_and_real_query_failure_is_reported() {
        let directory =
            crate::pages::runtime::logs::tests::settings_test_directory("home-history-cancel");
        let missing = directory.join("not-created.sqlite3");
        let store = TrafficHistoryStore::new(&missing);
        assert!(
            read_history(&store, 100_000_000, &AtomicU64::new(1), 0)
                .unwrap()
                .is_none()
        );
        assert!(!missing.exists());
        let invalid = TrafficHistoryStore::new(&directory);
        assert!(read_history(&invalid, 100_000_000, &AtomicU64::new(0), 0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
