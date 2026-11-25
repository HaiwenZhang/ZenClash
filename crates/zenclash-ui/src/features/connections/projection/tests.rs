use super::*;
use zenclash_core::{Connection, ConnectionsSnapshot};

#[test]
fn connection_duration_handles_clock_skew_and_invalid_start_time() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T12:10:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(connection_duration("2026-10-01T12:07:42Z", now), "02:18");
    assert_eq!(connection_duration("2026-10-01T12:11:00Z", now), "00:00");
    assert_eq!(connection_duration("unavailable", now), "—");
}

#[test]
fn snapshot_distributions_count_real_processes_and_normalize_protocols() {
    let mut browser = Connection::default();
    browser.metadata.process = "browser.exe".into();
    browser.metadata.network = "tcp".into();
    let mut unknown = Connection::default();
    unknown.metadata.network = "UDP".into();
    let (processes, protocols) = distributions(&[browser.clone(), browser, unknown]);
    assert_eq!(processes, [("browser.exe".into(), 2), (String::new(), 1)]);
    assert_eq!(protocols, [("TCP".into(), 2), ("UDP".into(), 1)]);
}

fn snapshot(host: &str) -> Arc<ConnectionsSnapshot> {
    let mut connection = Connection::default();
    connection.metadata.host = host.into();
    Arc::new(ConnectionsSnapshot {
        connections: vec![connection],
        ..Default::default()
    })
}

#[tokio::test]
async fn continuous_input_cancels_old_search_and_publishes_matching_snapshot() {
    let mut worker = ProjectionWorker::default();
    let runtime = tokio::runtime::Handle::current();
    let (old, first) = worker.start(
        &runtime,
        snapshot("old"),
        "old".into(),
        ConnectionTransport::All,
        ConnectionSort::Default,
    );
    let source = snapshot("new");
    let (latest, second) = worker.start(
        &runtime,
        source.clone(),
        "new".into(),
        ConnectionTransport::All,
        ConnectionSort::Default,
    );
    assert!(first.await.err().is_some_and(|error| error.is_cancelled()));
    let projection = second.await.unwrap().unwrap().unwrap();
    assert!(!worker.is_current(old));
    assert!(worker.is_current(latest));
    assert!(Arc::ptr_eq(&projection.snapshot, &source));
    assert_eq!(projection.order, [0]);
    assert_eq!(projection.query, "new");
}

#[tokio::test]
async fn completed_result_cannot_replace_a_newer_search() {
    let mut worker = ProjectionWorker::default();
    let runtime = tokio::runtime::Handle::current();
    let (old, task) = worker.start(
        &runtime,
        snapshot("old"),
        "old".into(),
        ConnectionTransport::All,
        ConnectionSort::Default,
    );
    let completed = task.await.unwrap().unwrap().unwrap();
    let (new, task) = worker.start(
        &runtime,
        snapshot("new"),
        "missing".into(),
        ConnectionTransport::All,
        ConnectionSort::Default,
    );
    assert!(!worker.is_current(old));
    assert_eq!(completed.snapshot.connections[0].metadata.host, "old");
    let current = task.await.unwrap().unwrap().unwrap();
    assert!(worker.is_current(new));
    assert!(current.order.is_empty());
    assert_eq!(current.snapshot.connections[0].metadata.host, "new");
}

#[tokio::test]
async fn releasing_the_page_prevents_a_pending_search_from_publishing() {
    let mut worker = ProjectionWorker::default();
    let (generation, task) = worker.start(
        &tokio::runtime::Handle::current(),
        snapshot("node"),
        String::new(),
        ConnectionTransport::All,
        ConnectionSort::Default,
    );
    worker.cancel();
    assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
    assert!(!worker.is_current(generation));
}
