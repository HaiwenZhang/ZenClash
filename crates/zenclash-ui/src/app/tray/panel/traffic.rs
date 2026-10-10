use std::collections::VecDeque;

use gpui_kit::SharedString;
use zenclash_core::TrafficSnapshot;

const WINDOW_MS: u64 = 120_000;
const SAMPLE_LIMIT: usize = 300;

#[derive(Clone)]
pub(super) struct TrafficPoint {
    pub(super) at_ms: u64,
    pub(super) upload: f64,
    pub(super) download: f64,
}

impl TrafficPoint {
    pub(super) fn label(&self, end: u64) -> SharedString {
        if self.at_ms == end {
            zenclash_i18n::text("home.traffic.now").into()
        } else {
            format!("−{}s", end.saturating_sub(self.at_ms).div_ceil(1_000)).into()
        }
    }
}

/// Bounded stream history retained across panel dismissal, independent of UI redraws.
#[derive(Default)]
pub(in crate::app) struct TrafficHistory {
    generation: u64,
    samples: VecDeque<TrafficPoint>,
}

impl TrafficHistory {
    pub(in crate::app) fn observe(&mut self, snapshot: &TrafficSnapshot) {
        if self.generation != snapshot.generation {
            self.generation = snapshot.generation;
            self.samples.clear();
        }
        if snapshot.updated_at_ms == 0
            || self
                .samples
                .back()
                .is_some_and(|point| point.at_ms >= snapshot.updated_at_ms)
        {
            return;
        }
        while self
            .samples
            .front()
            .is_some_and(|point| snapshot.updated_at_ms.saturating_sub(point.at_ms) > WINDOW_MS)
            || self.samples.len() >= SAMPLE_LIMIT
        {
            self.samples.pop_front();
        }
        self.samples.push_back(TrafficPoint {
            at_ms: snapshot.updated_at_ms,
            upload: snapshot.upload as f64,
            download: snapshot.download as f64,
        });
    }

    pub(super) fn points(&self, generation: u64) -> Vec<TrafficPoint> {
        if self.generation == generation {
            self.samples.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at_ms: u64, generation: u64) -> TrafficSnapshot {
        TrafficSnapshot {
            updated_at_ms: at_ms,
            generation,
            upload: 1024,
            download: 2048,
            ..Default::default()
        }
    }

    #[test]
    fn redraws_and_disconnects_do_not_add_samples_and_generations_do_not_mix() {
        let mut history = TrafficHistory::default();
        history.observe(&frame(1_000, 1));
        history.observe(&frame(1_000, 1));
        assert_eq!(history.points(1).len(), 1);
        history.observe(&frame(2_000, 1));
        assert_eq!(history.points(1).len(), 2);
        assert_eq!(history.points(1)[0].download, 2048.);
        history.observe(&frame(0, 2));
        assert!(history.points(1).is_empty());
        assert!(history.points(2).is_empty());
    }

    #[test]
    fn history_is_bounded_by_time_and_count() {
        let mut history = TrafficHistory::default();
        for at_ms in 1..=400 {
            history.observe(&frame(at_ms, 1));
        }
        assert_eq!(history.points(1).len(), SAMPLE_LIMIT);
        history.observe(&frame(121_000, 1));
        assert_eq!(history.points(1).len(), 1);
    }
}
