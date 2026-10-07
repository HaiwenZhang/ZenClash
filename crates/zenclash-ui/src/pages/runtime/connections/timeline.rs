use std::{collections::HashMap, sync::Arc, time::Instant};

use zenclash_core::ConnectionsSnapshot;

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
            let closed = Arc::make_mut(&mut self.closed);
            let mut newly_closed = 0;
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
                } else if newly_closed < 1000 {
                    newly_closed += 1;
                    closed
                        .connections
                        .retain(|connection| connection.id != before.id);
                    closed.connections.insert(0, before.clone());
                    self.durations.insert(
                        before.id.clone(),
                        super::projection::connection_duration(&before.start, chrono::Utc::now()),
                    );
                }
            }
            closed.connections.truncate(1000);
            let retained = closed
                .connections
                .iter()
                .map(|connection| connection.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            self.durations
                .retain(|id, _| retained.contains(id.as_str()));
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
}
