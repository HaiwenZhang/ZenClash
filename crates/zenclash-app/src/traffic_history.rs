//! Owned background traffic-history sampling and bounded SQLite persistence.

use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use zenclash_core::{
    AppPreferences, MihomoClient, TrafficDeltaLogger, TrafficHistoryEntry, TrafficHistoryStore,
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const FLUSH_ENTRY_THRESHOLD: usize = 1_000;
const FLUSH_BATCH_ENTRIES: usize = 10_000;
const MAX_PENDING_ENTRIES: usize = 5_000;
const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

/// Lock-free policy snapshot shared with the Tokio traffic accounting task.
#[derive(Debug)]
struct TrafficHistoryPolicy {
    enabled: AtomicBool,
    retention_days: AtomicU16,
}

impl TrafficHistoryPolicy {
    fn new(preferences: &AppPreferences) -> Self {
        Self {
            enabled: AtomicBool::new(preferences.traffic_history_enabled),
            retention_days: AtomicU16::new(preferences.traffic_retention_days.max(1)),
        }
    }

    fn update(&self, preferences: &AppPreferences) {
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
        if self.policy.enabled() && self.store.path().is_file() {
            let _ = flush(
                &self.store,
                &mut self.pending,
                self.policy.cutoff_ms(unix_millis()),
            )
            .await;
        }
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
                    let _ = self.flush_periodic(now_ms).await;
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
                let _ = self.flush_periodic(now_ms).await;
            }
        }
        self
    }

    /// Flushes queued traffic deltas on periodic ticks, skipping SQLite I/O
    /// entirely when `pending` is empty to guarantee zero idle disk writes.
    async fn flush_periodic(&mut self, now_ms: u64) -> Result<bool, String> {
        self.last_flush = Instant::now();
        if self.pending.is_empty() {
            return Ok(false);
        }
        flush(
            &self.store,
            &mut self.pending,
            self.policy.cutoff_ms(now_ms),
        )
        .await?;
        Ok(true)
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
mod tests;
