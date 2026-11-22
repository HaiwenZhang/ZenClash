use super::*;
use std::time::Duration;

fn sample(upload: u64, download: u64) -> Arc<ConnectionsSnapshot> {
    Arc::new(ConnectionsSnapshot {
        connections: vec![zenclash_core::Connection {
            id: "one".into(),
            upload,
            download,
            ..Default::default()
        }],
        ..Default::default()
    })
}

#[test]
fn rates_need_two_samples_and_use_elapsed_time() {
    let mut timeline = ConnectionTimeline::default();
    let now = Instant::now();
    let first = sample(100, 200);
    timeline.observe(1, first.clone(), now);
    assert_eq!(timeline.rates("one"), None);
    timeline.observe(1, first, now + Duration::from_secs(1));
    timeline.observe(1, sample(500, 1000), now + Duration::from_secs(2));
    assert_eq!(timeline.rates("one"), Some([200, 400]));
}

#[test]
fn disappearing_connections_are_retained_until_generation_changes() {
    let mut timeline = ConnectionTimeline::default();
    let now = Instant::now();
    timeline.observe(1, sample(100, 200), now);
    timeline.observe(
        1,
        Arc::new(ConnectionsSnapshot::default()),
        now + Duration::from_secs(1),
    );
    assert_eq!(timeline.closed().connections[0].upload, 100);
    timeline.observe(2, sample(0, 0), now + Duration::from_secs(2));
    assert!(timeline.closed().connections.is_empty());
    assert_eq!(timeline.rates("one"), None);
}

fn batch(items: impl IntoIterator<Item = (&'static str, u64)>) -> Arc<ConnectionsSnapshot> {
    Arc::new(ConnectionsSnapshot {
        connections: items
            .into_iter()
            .map(|(id, upload)| zenclash_core::Connection {
                id: id.into(),
                upload,
                start: "2026-01-01T00:00:00Z".into(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

#[test]
fn closed_connection_merge_preserves_order_deduplicates_and_caps_at_limit() {
    let mut timeline = ConnectionTimeline::default();
    let now = Instant::now();

    timeline.observe(1, batch([("a", 10), ("b", 20), ("c", 30)]), now);
    timeline.observe(1, batch([("c", 40)]), now + Duration::from_secs(1));
    let closed = timeline.closed();
    assert_eq!(
        closed
            .connections
            .iter()
            .map(|connection| (connection.id.as_str(), connection.upload))
            .collect::<Vec<_>>(),
        [("b", 20), ("a", 10)]
    );
    assert!(timeline.duration("a").is_some());
    assert!(timeline.duration("b").is_some());

    // No connections closing should avoid cloning the closed snapshot Arc.
    timeline.observe(1, batch([("c", 50)]), now + Duration::from_secs(2));
    assert!(Arc::ptr_eq(&closed, &timeline.closed()));

    // Re-closing an existing ID moves it to the front and replaces the older record.
    timeline.observe(
        1,
        batch([("a", 99), ("d", 40)]),
        now + Duration::from_secs(3),
    );
    timeline.observe(
        1,
        Arc::new(ConnectionsSnapshot::default()),
        now + Duration::from_secs(4),
    );
    assert_eq!(
        timeline
            .closed()
            .connections
            .iter()
            .map(|connection| (connection.id.as_str(), connection.upload))
            .collect::<Vec<_>>(),
        [("d", 40), ("a", 99), ("c", 50), ("b", 20)]
    );

    // Bulk close exceeding MAX_CLOSED_CONNECTIONS caps both closed list and cached durations.
    let bulk = Arc::new(ConnectionsSnapshot {
        connections: (0..1_200)
            .map(|index| zenclash_core::Connection {
                id: format!("bulk-{index:04}"),
                start: "2026-01-01T00:00:00Z".into(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    timeline.observe(1, bulk, now + Duration::from_secs(5));
    timeline.observe(
        1,
        Arc::new(ConnectionsSnapshot::default()),
        now + Duration::from_secs(6),
    );
    let closed = timeline.closed();
    assert_eq!(closed.connections.len(), MAX_CLOSED_CONNECTIONS);
    assert_eq!(closed.connections[0].id, "bulk-1199");
    assert_eq!(
        closed.connections[MAX_CLOSED_CONNECTIONS - 1].id,
        "bulk-0200"
    );
    assert!(timeline.duration("bulk-1199").is_some());
    assert!(timeline.duration("bulk-0199").is_none());
    assert!(timeline.duration("a").is_none());
}
