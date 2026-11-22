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
mod tests;
