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
async fn idle_periodic_flush_skips_sqlite_writes_when_pending_is_empty() {
    let database = TestDatabase::new("idle-skip");
    let now = unix_millis();
    let policy = Arc::new(TrafficHistoryPolicy::new(&AppPreferences::default()));
    let mut recorder = HistoryRecorder {
        client: MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
            .unwrap(),
        store: database.store.clone(),
        policy,
        logger: TrafficDeltaLogger::new(now),
        pending: Vec::new(),
        was_enabled: true,
        last_flush: Instant::now(),
    };

    assert!(!recorder.flush_periodic(now).await.unwrap());
    assert!(
        !database.store.path().exists(),
        "idle periodic tick must not open or write to SQLite when pending is empty"
    );

    recorder.pending.push(TrafficHistoryEntry {
        timestamp_ms: now,
        source_ip: "fixture".into(),
        host: "fixture".into(),
        outbound: "DIRECT".into(),
        process: "fixture".into(),
        upload: 12,
        download: 34,
    });
    assert!(recorder.flush_periodic(now).await.unwrap());
    assert!(database.store.path().exists());
    assert!(recorder.pending.is_empty());
    let totals = database
        .store
        .overview(&TrafficHistoryQuery {
            start_ms: now,
            end_ms: now,
            bucket_ms: 1,
            dimension: Default::default(),
        })
        .unwrap()
        .totals;
    assert_eq!(
        (totals.samples, totals.upload, totals.download),
        (1, 12, 34)
    );
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
    use tokio::io::AsyncWriteExt;

    let database = TestDatabase::new("final-flush");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (inflight, request_started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        for sample in 1..=3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            let count = request.len();
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
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
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

mod cancellation_tests {
    use std::{future::Future, task::Poll};

    use zenclash_core::TrafficHistoryQuery;

    use super::super::*;

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
