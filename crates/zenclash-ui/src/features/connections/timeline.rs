use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use zenclash_core::ConnectionsSnapshot;

const MAX_CLOSED_CONNECTIONS: usize = 1000;

/// Bounded, session-local observations; missing samples never imply zero traffic.
#[derive(Default)]
pub(super) struct ConnectionTimeline {
    generation: Option<u64>,
    previous: Option<(Arc<ConnectionsSnapshot>, Instant)>,
    closed: Arc<ConnectionsSnapshot>,
    durations: HashMap<String, String>,
    rates: HashMap<String, [u64; 2]>,
}

impl ConnectionTimeline {
    pub(super) fn observe(
        &mut self,
        generation: u64,
        snapshot: Arc<ConnectionsSnapshot>,
        now: Instant,
    ) {
        if self.generation != Some(generation) {
            *self = Self::default();
            self.generation = Some(generation);
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|(previous, _)| Arc::ptr_eq(previous, &snapshot))
        {
            return;
        }
        self.rates.clear();
        if let Some((previous, observed)) = &self.previous {
            let elapsed = now.saturating_duration_since(*observed).as_millis();
            let live = snapshot
                .connections
                .iter()
                .map(|connection| (connection.id.as_str(), connection))
                .collect::<HashMap<_, _>>();
            let mut newly_closed = Vec::new();
            for before in &previous.connections {
                if let Some(after) = live.get(before.id.as_str()) {
                    if elapsed > 0
                        && after.upload >= before.upload
                        && after.download >= before.download
                    {
                        let per_second = |delta: u64| {
                            u64::try_from(u128::from(delta) * 1000 / elapsed).unwrap_or(u64::MAX)
                        };
                        self.rates.insert(
                            before.id.clone(),
                            [
                                per_second(after.upload - before.upload),
                                per_second(after.download - before.download),
                            ],
                        );
                    }
                } else {
                    newly_closed.push(before);
                }
            }
            if !newly_closed.is_empty() {
                let closed_at = chrono::Utc::now();
                let closed = Arc::make_mut(&mut self.closed);
                let previous_closed = std::mem::take(&mut closed.connections);
                let capacity =
                    (newly_closed.len() + previous_closed.len()).min(MAX_CLOSED_CONNECTIONS);
                let mut merged = Vec::with_capacity(capacity);
                let mut seen = HashSet::with_capacity(capacity);
                for before in newly_closed.into_iter().rev() {
                    if merged.len() == MAX_CLOSED_CONNECTIONS {
                        break;
                    }
                    if seen.insert(before.id.clone()) {
                        self.durations.insert(
                            before.id.clone(),
                            super::projection::connection_duration(&before.start, closed_at),
                        );
                        merged.push(before.clone());
                    }
                }
                for existing in previous_closed {
                    if merged.len() == MAX_CLOSED_CONNECTIONS {
                        break;
                    }
                    if seen.insert(existing.id.clone()) {
                        merged.push(existing);
                    }
                }
                closed.connections = merged;
                self.durations.retain(|id, _| seen.contains(id));
            }
        }
        self.previous = Some((snapshot, now));
    }

    pub(super) fn closed(&self) -> Arc<ConnectionsSnapshot> {
        self.closed.clone()
    }
    pub(super) fn rates(&self, id: &str) -> Option<[u64; 2]> {
        self.rates.get(id).copied()
    }
    pub(super) fn duration(&self, id: &str) -> Option<&str> {
        self.durations.get(id).map(String::as_str)
    }
    pub(super) fn active_count(&self) -> usize {
        self.previous
            .as_ref()
            .map_or(0, |(snapshot, _)| snapshot.connections.len())
    }
}

#[cfg(test)]
mod tests {
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
}
