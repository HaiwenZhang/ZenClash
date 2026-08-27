use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use zenclash_core::{AppPreferences, TrafficDeltaLogger, TrafficHistoryEntry, TrafficHistoryStore};

use super::ZenClashApp;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const FLUSH_ENTRY_THRESHOLD: usize = 5_000;
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

impl ZenClashApp {
    pub(super) fn start_traffic_history(&self, store: Option<TrafficHistoryStore>) {
        let Some(store) = store else {
            return;
        };
        let client = self.client.clone();
        let policy = Arc::clone(&self.traffic_history_policy);
        let task = self.runtime.spawn(async move {
            let started_at = unix_millis();
            let mut logger = TrafficDeltaLogger::new(started_at);
            let mut was_enabled = policy.enabled();
            let mut pending = Vec::new();
            let mut last_flush = Instant::now();
            flush(&store, &mut pending, policy.cutoff_ms(started_at)).await;

            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let now_ms = unix_millis();
                let enabled = policy.enabled();
                if !enabled {
                    if was_enabled {
                        flush(&store, &mut pending, policy.cutoff_ms(now_ms)).await;
                        logger.reset(now_ms);
                        was_enabled = false;
                    }
                    continue;
                }
                if !was_enabled {
                    logger.reset(now_ms);
                    was_enabled = true;
                }

                match client.connections_snapshot().await {
                    Ok(snapshot) => pending.extend(logger.observe(&snapshot, now_ms)),
                    Err(error) => {
                        tracing::debug!(%error, "failed to sample core connections for traffic history");
                    }
                }
                if last_flush.elapsed() >= FLUSH_INTERVAL
                    || pending.len() >= FLUSH_ENTRY_THRESHOLD
                {
                    flush(&store, &mut pending, policy.cutoff_ms(now_ms)).await;
                    last_flush = Instant::now();
                }
            }
        });
        drop(task);
    }
}

async fn flush(
    store: &TrafficHistoryStore,
    pending: &mut Vec<TrafficHistoryEntry>,
    cutoff_ms: u64,
) {
    let batch = mem::take(pending);
    let database = store.clone();
    match tokio::task::spawn_blocking(move || {
        let result = database.insert_and_cleanup(&batch, cutoff_ms);
        (batch, result)
    })
    .await
    {
        Ok((_, Ok(()))) => {}
        Ok((mut batch, Err(error))) => {
            tracing::warn!(%error, "failed to persist traffic-history batch");
            batch.append(pending);
            *pending = batch;
        }
        Err(error) => {
            tracing::warn!(%error, "traffic-history blocking task failed");
        }
    }
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
    use super::*;

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
}
