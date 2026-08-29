use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{SecondsFormat, TimeZone, Utc};

use super::*;
use crate::{TrafficAccountingConnection, TrafficAccountingMetadata, TrafficAccountingSnapshot};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn test_database(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "zenclash-traffic-{name}-{}-{sequence}.sqlite3",
        std::process::id()
    ))
}

fn connection(id: &str, start_ms: i64, upload: u64, download: u64) -> TrafficAccountingConnection {
    TrafficAccountingConnection {
        id: id.into(),
        start: Utc
            .timestamp_millis_opt(start_ms)
            .single()
            .expect("test timestamp is valid")
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        upload,
        download,
        outbound: "香港 01".into(),
        metadata: TrafficAccountingMetadata {
            source_ip: "192.168.1.8".into(),
            host: "example.com".into(),
            destination_ip: "93.184.216.34".into(),
            process: "curl".into(),
        },
    }
}

fn snapshot(
    connections: Vec<TrafficAccountingConnection>,
    upload_total: u64,
    download_total: u64,
) -> TrafficAccountingSnapshot {
    TrafficAccountingSnapshot {
        connections,
        download_total,
        upload_total,
    }
}

fn entry(
    timestamp_ms: u64,
    source_ip: &str,
    host: &str,
    outbound: &str,
    process: &str,
    upload: u64,
    download: u64,
) -> TrafficHistoryEntry {
    TrafficHistoryEntry {
        timestamp_ms,
        source_ip: source_ip.into(),
        host: host.into(),
        outbound: outbound.into(),
        process: process.into(),
        upload,
        download,
    }
}

fn cleanup_database(path: &Path) {
    for suffix in ["", "-shm", "-wal"] {
        let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
        if let Err(error) = fs::remove_file(&candidate) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
    }
}

#[test]
fn logger_uses_baselines_and_only_persists_positive_real_deltas() {
    let mut logger = TrafficDeltaLogger::new(2_000);
    let old = connection("old", 1_000, 100, 200);
    assert!(
        logger
            .observe(&snapshot(vec![old.clone()], 100, 200), 2_100)
            .is_empty()
    );

    let mut old_updated = old;
    old_updated.upload = 140;
    old_updated.download = 260;
    let new = connection("new", 2_200, 7, 11);
    let deltas = logger.observe(&snapshot(vec![old_updated, new], 147, 271), 2_300);

    assert_eq!(deltas.len(), 2);
    assert_eq!((deltas[0].upload, deltas[0].download), (40, 60));
    assert_eq!((deltas[1].upload, deltas[1].download), (7, 11));
    assert_eq!(deltas[1].host, "example.com");
    assert_eq!(deltas[1].outbound, "香港 01");
}

#[test]
fn logger_reestablishes_baselines_after_mihomo_counter_reset() {
    let mut logger = TrafficDeltaLogger::new(1_000);
    let first = connection("same-id", 500, 100, 100);
    assert!(
        logger
            .observe(&snapshot(vec![first], 100, 100), 1_100)
            .is_empty()
    );

    let restarted = connection("same-id", 500, 5, 6);
    assert!(
        logger
            .observe(&snapshot(vec![restarted], 5, 6), 1_200)
            .is_empty()
    );

    let updated = connection("same-id", 500, 8, 10);
    let deltas = logger.observe(&snapshot(vec![updated], 8, 10), 1_300);
    assert_eq!((deltas[0].upload, deltas[0].download), (3, 4));
}

#[test]
fn logger_removes_baselines_for_connections_missing_from_the_latest_snapshot() {
    let mut logger = TrafficDeltaLogger::new(1_000);
    let old = connection("old", 500, 100, 100);
    let replacement = connection("replacement", 500, 0, 0);
    let _ = logger.observe(&snapshot(vec![old], 100, 100), 1_100);
    let _ = logger.observe(&snapshot(vec![replacement], 100, 100), 1_200);

    let returned = connection("old", 500, 140, 160);
    let deltas = logger.observe(&snapshot(vec![returned], 140, 160), 1_300);

    assert!(deltas.is_empty());
}

#[test]
fn sqlite_store_supports_cleanup_overview_and_drill_down() {
    let path = test_database("overview");
    let store = TrafficHistoryStore::new(&path);
    let entries = vec![
        entry(900, "old", "expired.example", "DIRECT", "old", 9, 9),
        entry(1_000, "Inner", "one.example", "香港 01", "curl", 10, 20),
        entry(
            1_200,
            "192.168.1.8",
            "one.example",
            "香港 01",
            "browser",
            5,
            15,
        ),
        entry(2_200, "Inner", "two.example", "DIRECT", "curl", 30, 10),
    ];
    store
        .insert_and_cleanup(&entries, 1_000)
        .expect("insert succeeds");
    let query = TrafficHistoryQuery {
        dimension: TrafficDimension::Host,
        start_ms: 1_000,
        end_ms: 2_999,
        bucket_ms: 1_000,
    };

    let overview = store.overview(&query).expect("overview succeeds");
    assert_eq!(
        overview.totals,
        TrafficTotals {
            upload: 45,
            download: 45,
            total: 90,
            samples: 3
        }
    );
    assert_eq!(overview.rankings.len(), 2);
    assert_eq!(overview.rankings[0].label, "one.example");
    assert_eq!(overview.rankings[0].total, 50);
    assert_eq!(overview.trend.len(), 2);
    assert_eq!(
        (overview.trend[0].upload, overview.trend[0].download),
        (15, 35)
    );
    assert_eq!(
        (overview.trend[1].upload, overview.trend[1].download),
        (30, 10)
    );

    let details = store
        .details(&query, "one.example")
        .expect("details succeed");
    assert_eq!(details.len(), 2);
    assert_eq!(details[0].label, "Inner");
    let proxy = store
        .proxy_stats(&query, "one.example", "Inner")
        .expect("proxy stats succeed");
    assert_eq!(proxy[0].label, "香港 01");
    assert_eq!(proxy[0].total, 30);

    store.clear().expect("clear succeeds");
    store
        .insert_and_cleanup(
            &[entry(
                2_500,
                "Inner",
                "pending.example",
                "DIRECT",
                "curl",
                99,
                99,
            )],
            0,
        )
        .expect("a pending pre-clear batch is safely ignored");
    assert_eq!(
        store.overview(&query).expect("empty overview").totals.total,
        0
    );
    cleanup_database(&path);
}

#[test]
fn sqlite_store_rejects_unbounded_trend_queries() {
    let path = test_database("bounds");
    let store = TrafficHistoryStore::new(&path);
    let query = TrafficHistoryQuery {
        dimension: TrafficDimension::Host,
        start_ms: 0,
        end_ms: 1_000_000,
        bucket_ms: 1,
    };

    assert!(matches!(
        store.overview(&query),
        Err(TrafficHistoryError::InvalidQuery(_))
    ));
    cleanup_database(&path);
}
