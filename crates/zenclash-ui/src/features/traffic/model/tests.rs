use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zenclash_core::{TrafficHistoryEntry, TrafficHistoryQuery};

struct HistoryFixture {
    root: PathBuf,
    store: TrafficHistoryStore,
}

impl HistoryFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "zenclash-history-model-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("history fixture directory: {error}"),
            }
        };
        let store = TrafficHistoryStore::new(root.join("history.sqlite"));
        store
            .insert_and_cleanup(
                &[TrafficHistoryEntry {
                    timestamp_ms: unix_millis(),
                    host: "history.example".into(),
                    process: "fixture".into(),
                    source_ip: "127.0.0.1".into(),
                    outbound: "DIRECT".into(),
                    upload: 7,
                    download: 11,
                }],
                0,
            )
            .unwrap();
        Self { root, store }
    }

    fn model(&self) -> TrafficHistory {
        TrafficHistory::new(tokio::runtime::Handle::current(), Some(self.store.clone()))
    }

    fn overview_query(&self) -> TrafficHistoryQuery {
        let end_ms = unix_millis();
        TrafficHistoryQuery {
            dimension: TrafficDimension::Host,
            start_ms: end_ms.saturating_sub(60 * 60 * 1_000),
            end_ms,
            bucket_ms: 5 * 60 * 1_000,
        }
    }
}

impl Drop for HistoryFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn matching_query_counters_and_store_path_cannot_accept_another_models_read() {
    let fixture = HistoryFixture::new();
    let mut first = fixture.model();
    let mut second = fixture.model();
    let first_read = first.start_read().unwrap();
    let second_read = second.start_read().unwrap();
    assert_eq!(first.state.request_sequence, second.state.request_sequence);
    assert_eq!(first.state.query_revision, second.state.query_revision);
    assert_eq!(first.source(), second.source());
    assert!(!second.complete_read(first_read.finish().await));
    assert!(second.is_loading());
    assert!(second.presentation().is_none());
    assert!(!second.complete_read(second_read.finish().await));
    assert!(!second.is_loading());
    assert_eq!(second.presentation().unwrap().overview().totals.total, 18);
}

#[tokio::test]
async fn dropping_the_model_cancels_a_read_waiting_for_its_original_query_gate() {
    let fixture = HistoryFixture::new();
    let mut model = fixture.model();
    let held = model.state.query_gate.clone().lock_owned().await;
    let read = model.start_read().unwrap();
    drop(model);
    let completion = read.finish().await;
    assert!(completion.result.is_err());
    assert!(completion.owner.upgrade().is_none());
    drop(held);
    assert_eq!(
        fixture
            .store
            .overview(&fixture.overview_query())
            .unwrap()
            .totals
            .total,
        18
    );
}

#[tokio::test]
async fn an_owned_clear_finishes_after_the_presentation_model_is_dropped() {
    let fixture = HistoryFixture::new();
    let mut model = fixture.model();
    let held = model.state.query_gate.clone().lock_owned().await;
    let read = model.start_read().unwrap();
    let clear = model.prepare_clear().unwrap();
    assert!(
        model.is_loading(),
        "preparing a clear must not invalidate before admission"
    );
    model.invalidate_for_clear();
    assert!(read.finish().await.result.is_err());
    let task = clear.start();
    drop(model);
    drop(held);
    task.await.unwrap().unwrap();
    assert_eq!(
        fixture
            .store
            .overview(&fixture.overview_query())
            .unwrap()
            .totals
            .samples,
        0
    );
}
