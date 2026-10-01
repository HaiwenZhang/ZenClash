use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use zenclash_core::{AppPreferences, TrafficDeltaLogger, TrafficHistoryEntry, TrafficHistoryStore};

use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use zenclash_core::MihomoClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const FLUSH_ENTRY_THRESHOLD: usize = 1_000;
const FLUSH_BATCH_ENTRIES: usize = 10_000;
const MAX_PENDING_ENTRIES: usize = 5_000;
const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

/// Lock-free policy snapshot shared with the Tokio traffic accounting task.
#[derive(Debug)]
pub(super) struct TrafficHistoryPolicy {
    enabled: AtomicBool,
    retention_days: AtomicU16,
}

impl TrafficHistoryPolicy {
    pub(super) fn new(preferences: &AppPreferences) -> Self {
        Self {
            enabled: AtomicBool::new(preferences.traffic_history_enabled),
            retention_days: AtomicU16::new(preferences.traffic_retention_days.max(1)),
        }
    }

    pub(super) fn update(&self, preferences: &AppPreferences) {
        self.retention_days
            .store(preferences.traffic_retention_days.max(1), Ordering::Release);
        self.enabled
            .store(preferences.traffic_history_enabled, Ordering::Release);
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn cutoff_ms(&self, now_ms: u64) -> u64 {
        now_ms
            .saturating_sub(u64::from(self.retention_days.load(Ordering::Acquire)) * MILLIS_PER_DAY)
    }
}

/// Owns traffic sampling and preserves pending entries until shutdown flushes them.
///
/// Application exit must await `shutdown` before destroying the Tokio runtime.
/// Dropping the last owner cancels sampling; it cannot perform an asynchronous flush.
pub struct TrafficHistorySession {
    runtime: Handle,
    policy: Arc<TrafficHistoryPolicy>,
    cancellation: watch::Sender<bool>,
    state: tokio::sync::Mutex<HistoryTaskState>,
}

enum HistoryTaskState {
    Recording(JoinHandle<HistoryRecorder>),
    Flushing(JoinHandle<Result<(), (Box<HistoryRecorder>, String)>>),
    Stopped,
    Failed(String),
}

struct HistoryRecorder {
    client: MihomoClient,
    store: TrafficHistoryStore,
    policy: Arc<TrafficHistoryPolicy>,
    logger: TrafficDeltaLogger,
    pending: Vec<TrafficHistoryEntry>,
    was_enabled: bool,
    last_flush: Instant,
}

impl TrafficHistorySession {
    /// Starts one owned background sampler using the application's history policy.
    #[must_use]
    pub fn start(
        runtime: &Handle,
        client: MihomoClient,
        store: TrafficHistoryStore,
        preferences: &AppPreferences,
    ) -> Arc<Self> {
        let policy = Arc::new(TrafficHistoryPolicy::new(preferences));
        let recorder = HistoryRecorder {
            client,
            store,
            policy: Arc::clone(&policy),
            logger: TrafficDeltaLogger::new(unix_millis()),
            pending: Vec::new(),
            was_enabled: policy.enabled(),
            last_flush: Instant::now(),
        };
        Self::start_recorder(runtime, recorder)
    }

    fn start_recorder(runtime: &Handle, recorder: HistoryRecorder) -> Arc<Self> {
        let policy = Arc::clone(&recorder.policy);
        let (cancellation, receiver) = watch::channel(false);
        let task = runtime.spawn(recorder.run(receiver));
        Arc::new(Self {
            runtime: runtime.clone(),
            policy,
            cancellation,
            state: tokio::sync::Mutex::new(HistoryTaskState::Recording(task)),
        })
    }

    /// Updates the in-memory policy without touching the database or blocking the UI.
    pub fn update_preferences(&self, preferences: &AppPreferences) {
        self.policy.update(preferences);
    }

    /// Cancels sampling and waits for the final batch to commit.
    ///
    /// A database failure preserves pending entries and resumes sampling, allowing
    /// an application that remains open after failed quit to retry safely.
    ///
    /// # Errors
    ///
    /// Returns an error if the sampler fails or the final batch cannot be persisted.
    pub async fn shutdown(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        loop {
            match &mut *state {
                HistoryTaskState::Recording(task) => {
                    self.cancellation.send_replace(true);
                    // Keep the handle in the session while awaiting it. Cancelling
                    // this waiter must not detach or discard the returned recorder.
                    match task.await {
                        Ok(mut recorder) => {
                            let flush = self.runtime.spawn(async move {
                                match flush(
                                    &recorder.store,
                                    &mut recorder.pending,
                                    recorder.policy.cutoff_ms(unix_millis()),
                                )
                                .await
                                {
                                    Ok(()) => Ok(()),
                                    Err(error) => Err((Box::new(recorder), error)),
                                }
                            });
                            *state = HistoryTaskState::Flushing(flush);
                        }
                        Err(error) => {
                            let error = zenclash_i18n::text_with(
                                "traffic.errors.sampler_task",
                                &[("error", error.to_string())],
                            );
                            *state = HistoryTaskState::Failed(error.clone());
                            return Err(error);
                        }
                    }
                }
                HistoryTaskState::Flushing(task) => match task.await {
                    Ok(Ok(())) => {
                        *state = HistoryTaskState::Stopped;
                        return Ok(());
                    }
                    Ok(Err((recorder, error))) => {
                        self.cancellation.send_replace(false);
                        let task = self
                            .runtime
                            .spawn(recorder.run(self.cancellation.subscribe()));
                        *state = HistoryTaskState::Recording(task);
                        return Err(zenclash_i18n::text_with(
                            "traffic.errors.final_flush",
                            &[("error", error)],
                        ));
                    }
                    Err(error) => {
                        let error = zenclash_i18n::text_with(
                            "traffic.errors.sampler_task",
                            &[("error", error.to_string())],
                        );
                        *state = HistoryTaskState::Failed(error.clone());
                        return Err(error);
                    }
                },
                HistoryTaskState::Stopped => return Ok(()),
                HistoryTaskState::Failed(error) => return Err(error.clone()),
            }
        }
    }
}

impl Drop for TrafficHistorySession {
    fn drop(&mut self) {
        self.cancellation.send_replace(true);
        match self.state.get_mut() {
            HistoryTaskState::Recording(task) => task.abort(),
            HistoryTaskState::Flushing(task) => task.abort(),
            HistoryTaskState::Stopped | HistoryTaskState::Failed(_) => {}
        }
    }
}

impl HistoryRecorder {
    async fn run(mut self, mut cancellation: watch::Receiver<bool>) -> Self {
        let _ = flush(
            &self.store,
            &mut self.pending,
            self.policy.cutoff_ms(unix_millis()),
        )
        .await;
        loop {
            if *cancellation.borrow_and_update() {
                break;
            }
            tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow_and_update() { break; }
                    continue;
                }
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            let now_ms = unix_millis();
            let enabled = self.policy.enabled();
            if !enabled {
                if self.was_enabled {
                    let _ = flush(
                        &self.store,
                        &mut self.pending,
                        self.policy.cutoff_ms(now_ms),
                    )
                    .await;
                    self.logger.reset(now_ms);
                    self.was_enabled = false;
                }
                continue;
            }
            if !self.was_enabled {
                self.logger.reset(now_ms);
                self.was_enabled = true;
            }
            let snapshot = tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow_and_update() { break; }
                    continue;
                }
                snapshot = self.client.traffic_accounting_snapshot() => snapshot,
            };
            match snapshot {
                Ok(snapshot) => self.pending.extend(self.logger.observe(&snapshot, now_ms)),
                Err(error) => {
                    tracing::debug!(%error, "failed to sample core connections for traffic history")
                }
            }
            if self.last_flush.elapsed() >= FLUSH_INTERVAL
                || self.pending.len() >= FLUSH_ENTRY_THRESHOLD
            {
                let _ = flush(
                    &self.store,
                    &mut self.pending,
                    self.policy.cutoff_ms(now_ms),
                )
                .await;
                self.last_flush = Instant::now();
            }
        }
        self
    }
}

async fn flush(
    store: &TrafficHistoryStore,
    pending: &mut Vec<TrafficHistoryEntry>,
    cutoff_ms: u64,
) -> Result<(), String> {
    // Keep the uncommitted suffix accessible if the blocking worker fails.
    let batch = Arc::new(Mutex::new(mem::take(pending).into_iter()));
    let remaining = Arc::clone(&batch);
    let database = store.clone();
    let failure = match tokio::task::spawn_blocking(move || {
        let mut entries = remaining.lock();
        if entries.as_slice().is_empty() {
            return database.insert_and_cleanup(&[], cutoff_ms);
        }
        while !entries.as_slice().is_empty() {
            let count = entries.len().min(FLUSH_BATCH_ENTRIES);
            // Expensive retention cleanup runs once, after the last batch.
            let cutoff = if count == entries.len() { cutoff_ms } else { 0 };
            database.insert_and_cleanup(&entries.as_slice()[..count], cutoff)?;
            // Advance only after this batch's SQLite transaction commits.
            let _ = entries.nth(count - 1);
        }
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => {
            tracing::warn!(%error, "failed to persist traffic-history batch");
            error.to_string()
        }
        Err(error) => {
            tracing::warn!(%error, "traffic-history blocking task failed");
            error.to_string()
        }
    };
    let mut remaining = batch.lock().by_ref().collect::<Vec<_>>();
    remaining.append(pending);
    let dropped = trim_pending_entries(&mut remaining);
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "discarded oldest unpersisted traffic-history entries"
        );
    }
    *pending = remaining;
    Err(failure)
}

fn trim_pending_entries(pending: &mut Vec<TrafficHistoryEntry>) -> usize {
    let dropped = pending.len().saturating_sub(MAX_PENDING_ENTRIES);
    if dropped > 0 {
        pending.drain(..dropped);
    }
    dropped
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use zenclash_core::TrafficHistoryQuery;

    use super::*;

    struct TestDatabase {
        root: PathBuf,
        store: TrafficHistoryStore,
    }

    impl TestDatabase {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "zenclash-history-{label}-{}-{timestamp}",
                std::process::id(),
            ));
            Self {
                store: TrafficHistoryStore::new(root.join("traffic.sqlite3")),
                root,
            }
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn entries(count: usize) -> Vec<TrafficHistoryEntry> {
        (0..count)
            .map(|index| TrafficHistoryEntry {
                timestamp_ms: 1_000 + index as u64,
                source_ip: "fixture".into(),
                host: "fixture".into(),
                outbound: "DIRECT".into(),
                process: "fixture".into(),
                upload: 1 + index as u64,
                download: 0,
            })
            .collect()
    }

    fn assert_persisted_sequence(store: &TrafficHistoryStore, count: usize) {
        let totals = store
            .overview(&TrafficHistoryQuery {
                start_ms: 0,
                end_ms: 1_000 + count as u64,
                bucket_ms: 1_001 + count as u64,
                dimension: Default::default(),
            })
            .unwrap()
            .totals;
        assert_eq!(totals.samples, count as u64);
        assert_eq!(totals.upload, count as u64 * (count as u64 + 1) / 2);
        for start in (0..count).step_by(512) {
            let end = (start + 512).min(count);
            let trend = store
                .overview(&TrafficHistoryQuery {
                    start_ms: 1_000 + start as u64,
                    end_ms: 999 + end as u64,
                    bucket_ms: 1,
                    dimension: Default::default(),
                })
                .unwrap()
                .trend;
            assert_eq!(trend.len(), end - start);
            for (point, index) in trend.iter().zip(start..end) {
                assert_eq!(
                    (point.timestamp_ms, point.upload),
                    (1_000 + index as u64, 1 + index as u64)
                );
            }
        }
    }

    #[tokio::test]
    async fn flush_persists_every_entry_across_sqlite_batch_boundaries() {
        for count in [10_001, 20_003] {
            let database = TestDatabase::new("large-batch");
            let mut pending = entries(count);

            flush(&database.store, &mut pending, 0).await.unwrap();

            assert!(pending.is_empty());
            assert_persisted_sequence(&database.store, count);
        }
    }

    #[tokio::test]
    async fn failed_later_batch_retries_only_uncommitted_entries() {
        let database = TestDatabase::new("partial-failure");
        let mut pending = entries(10_003);
        pending[10_001].upload = u64::MAX;

        assert!(flush(&database.store, &mut pending, 0).await.is_err());

        assert_eq!(pending.len(), 3);
        assert_eq!(
            pending
                .iter()
                .map(|entry| entry.timestamp_ms)
                .collect::<Vec<_>>(),
            vec![11_000, 11_001, 11_002],
        );
        assert_persisted_sequence(&database.store, 10_000);
        pending[1].upload = 10_002;

        flush(&database.store, &mut pending, 0).await.unwrap();

        assert!(pending.is_empty());
        assert_persisted_sequence(&database.store, 10_003);
    }

    #[tokio::test]
    async fn multi_batch_and_empty_flush_apply_the_retention_cutoff() {
        let database = TestDatabase::new("retention");
        let mut pending = entries(10_001);

        flush(&database.store, &mut pending, 1_001).await.unwrap();

        let query = TrafficHistoryQuery {
            start_ms: 0,
            end_ms: 20_000,
            bucket_ms: 20_001,
            dimension: Default::default(),
        };
        assert_eq!(
            database.store.overview(&query).unwrap().totals.samples,
            10_000
        );

        flush(&database.store, &mut pending, 20_000).await.unwrap();

        assert_eq!(database.store.overview(&query).unwrap().totals.samples, 0);
    }

    #[test]
    fn shared_policy_applies_restored_enablement_and_retention() {
        let mut preferences = AppPreferences::default();
        let policy = TrafficHistoryPolicy::new(&preferences);
        assert!(policy.enabled());
        assert_eq!(policy.cutoff_ms(100 * MILLIS_PER_DAY), 70 * MILLIS_PER_DAY);

        preferences.traffic_history_enabled = false;
        preferences.traffic_retention_days = 90;
        policy.update(&preferences);

        assert!(!policy.enabled());
        assert_eq!(policy.cutoff_ms(100 * MILLIS_PER_DAY), 10 * MILLIS_PER_DAY);
    }

    #[tokio::test]
    async fn failed_history_flush_retains_only_the_latest_bounded_entries() {
        let database = TestDatabase::new("failure-budget");
        let mut pending = entries(15_003);
        pending[10_001].upload = u64::MAX;

        assert!(flush(&database.store, &mut pending, 0).await.is_err());

        assert_eq!(
            (
                pending.len(),
                pending.first().unwrap().timestamp_ms,
                pending.last().unwrap().timestamp_ms,
            ),
            (MAX_PENDING_ENTRIES, 11_003, 16_002),
        );
        assert_persisted_sequence(&database.store, 10_000);
    }

    #[tokio::test]
    async fn shutdown_cancels_an_inflight_sample_and_commits_the_pending_delta() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let database = TestDatabase::new("final-flush");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (inflight, request_started) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            for sample in 1..=3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).await.unwrap();
                assert!(request[..count].starts_with(b"GET /connections "));
                if sample == 3 {
                    inflight.send(()).unwrap();
                    std::future::pending::<()>().await;
                    return;
                }
                let body = format!(
                    "{{\"connections\":[{{\"id\":\"fixture\",\"start\":\"1970-01-01T00:00:01Z\",\"upload\":{},\"metadata\":{{\"host\":\"fixture\"}}}}],\"uploadTotal\":{}}}",
                    sample * 7,
                    sample * 7,
                );
                stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len(),
                ).as_bytes()).await.unwrap();
            }
        });
        let client = MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            format!("http://{address}"),
            "",
        ))
        .unwrap();
        let started = unix_millis();
        let session = TrafficHistorySession::start(
            &Handle::current(),
            client,
            database.store.clone(),
            &AppPreferences::default(),
        );
        tokio::time::timeout(Duration::from_secs(6), request_started)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), session.shutdown())
            .await
            .unwrap()
            .unwrap();
        session.shutdown().await.unwrap();
        server.abort();
        let totals = database
            .store
            .overview(&TrafficHistoryQuery {
                start_ms: started,
                end_ms: unix_millis(),
                bucket_ms: 10_000,
                dimension: Default::default(),
            })
            .unwrap()
            .totals;
        assert_eq!((totals.samples, totals.upload), (1, 7));
    }

    #[tokio::test]
    async fn failed_shutdown_retains_pending_entries_for_a_successful_retry() {
        let database = TestDatabase::new("final-flush-retry");
        fs::create_dir_all(database.store.path()).unwrap();
        let now = unix_millis();
        let policy = Arc::new(TrafficHistoryPolicy::new(&AppPreferences::default()));
        let recorder = HistoryRecorder {
            client: MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap(),
            store: database.store.clone(),
            policy,
            logger: TrafficDeltaLogger::new(now),
            pending: entries(3)
                .into_iter()
                .map(|mut entry| {
                    entry.timestamp_ms += now;
                    entry
                })
                .collect(),
            was_enabled: true,
            last_flush: Instant::now(),
        };
        let session = TrafficHistorySession::start_recorder(&Handle::current(), recorder);
        assert!(session.shutdown().await.is_err());
        fs::remove_dir(database.store.path()).unwrap();
        session.shutdown().await.unwrap();
        session.shutdown().await.unwrap();
        let totals = database
            .store
            .overview(&TrafficHistoryQuery {
                start_ms: now,
                end_ms: now + 2_000,
                bucket_ms: 2_001,
                dimension: Default::default(),
            })
            .unwrap()
            .totals;
        assert_eq!((totals.samples, totals.upload), (3, 6));
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::future::Future;
    use std::task::Poll;
    use zenclash_core::TrafficHistoryQuery;

    #[tokio::test]
    async fn cancelling_a_shutdown_waiter_keeps_the_final_flush_owned() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-history-cancelled-waiter-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let store = TrafficHistoryStore::new(root.join("history.sqlite"));
        let now = unix_millis();
        let policy = Arc::new(TrafficHistoryPolicy::new(&AppPreferences::default()));
        let recorder = HistoryRecorder {
            client: MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap(),
            store: store.clone(),
            policy: policy.clone(),
            logger: TrafficDeltaLogger::new(now),
            pending: vec![TrafficHistoryEntry {
                timestamp_ms: now,
                source_ip: "fixture".into(),
                host: "fixture".into(),
                outbound: "DIRECT".into(),
                process: "fixture".into(),
                upload: 7,
                download: 0,
            }],
            was_enabled: true,
            last_flush: Instant::now(),
        };
        let (release, admitted_work) = tokio::sync::oneshot::channel();
        // Hold the sampler at an admitted-work boundary while its shutdown waiter is cancelled.
        let task = tokio::spawn(async move {
            admitted_work.await.unwrap();
            recorder
        });
        let (cancellation, _) = watch::channel(false);
        let session = TrafficHistorySession {
            runtime: Handle::current(),
            policy,
            cancellation,
            state: tokio::sync::Mutex::new(HistoryTaskState::Recording(task)),
        };
        let mut shutdown = Box::pin(session.shutdown());
        std::future::poll_fn(|cx| {
            assert!(shutdown.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(shutdown);
        release.send(()).unwrap();
        session.shutdown().await.unwrap();
        let totals = store
            .overview(&TrafficHistoryQuery {
                start_ms: now,
                end_ms: now,
                bucket_ms: 1,
                dimension: Default::default(),
            })
            .unwrap()
            .totals;
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(
            (totals.samples, totals.upload),
            (1, 7),
            "cancelled waiter lost the recorder and pending final batch"
        );
    }
}
