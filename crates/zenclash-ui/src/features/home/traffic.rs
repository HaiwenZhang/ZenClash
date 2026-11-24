use super::*;
use zenclash_core::TimedTrafficSample;
#[cfg(test)]
use zenclash_core::TrafficSnapshot;

const HOME_CHART_SAMPLE_LIMIT: usize = zenclash_core::LIVE_TRAFFIC_HISTORY_LIMIT;
const HOME_CHART_WINDOW_SECONDS: u64 = 120;

pub(super) fn observed_span_fraction(seconds: u64) -> f32 {
    (seconds.min(HOME_CHART_WINDOW_SECONDS) as f32 / HOME_CHART_WINDOW_SECONDS as f32)
        .max(1. / HOME_CHART_WINDOW_SECONDS as f32)
}

#[derive(Clone, Copy)]
struct TimedSample {
    at_ms: u64,
    value: TrafficSample,
}

#[derive(Default)]
pub(super) struct HomeChartState {
    generation: u64,
    traffic_revision: Option<u64>,
    samples: VecDeque<TimedSample>,
}

impl HomeChartState {
    #[cfg(test)]
    pub(super) fn prepare_design_validation(&mut self, generation: u64) -> TrafficSnapshot {
        *self = Self::default();
        let mut snapshot = TrafficSnapshot::default();
        for index in 0..=60 {
            let wave = ((index as f64 * 0.45).sin() + 1.) * 0.5;
            snapshot = TrafficSnapshot {
                generation,
                updated_at_ms: 1_700_000_000_000 + index * 5_000,
                upload: 20_000 + (wave * 60_000.) as u64,
                download: 80_000 + (wave * 160_000.) as u64,
                connected: true,
                ..Default::default()
            };
            self.observe(&snapshot, generation);
        }
        snapshot
    }

    fn synchronize_generation(&mut self, generation: u64) -> bool {
        let generation_changed = self.generation != generation;
        if generation_changed {
            self.generation = generation;
            // Rates are historical observations, not current configuration facts.
        }
        generation_changed
    }

    #[cfg(test)]
    fn observe(&mut self, snapshot: &TrafficSnapshot, generation: u64) -> bool {
        let generation_changed = self.synchronize_generation(generation);
        if snapshot.generation != generation || snapshot.updated_at_ms == 0 {
            return generation_changed;
        }
        self.append_sample(TimedTrafficSample::new(
            snapshot.updated_at_ms,
            TrafficSample {
                upload: snapshot.upload,
                download: snapshot.download,
            },
        )) || generation_changed
    }

    fn observe_samples(&mut self, samples: &VecDeque<TimedTrafficSample>, generation: u64) -> bool {
        let mut changed = self.synchronize_generation(generation);
        for sample in samples {
            changed |= self.append_sample(*sample);
        }
        changed
    }

    fn observe_monitor(
        &mut self,
        monitor: &zenclash_core::TrafficMonitor,
        generation: u64,
    ) -> bool {
        let revision = monitor.revision();
        if self.traffic_revision == Some(revision) {
            return self.synchronize_generation(generation);
        }
        self.traffic_revision = Some(revision);
        self.observe_samples(&monitor.timed_samples(), generation)
    }

    fn append_sample(&mut self, sample: TimedTrafficSample) -> bool {
        if self
            .samples
            .back()
            .is_some_and(|previous| previous.at_ms >= sample.timestamp_ms())
        {
            return false;
        }
        if self.samples.len() == HOME_CHART_SAMPLE_LIMIT {
            self.samples.pop_front();
        }
        self.samples.push_back(TimedSample {
            at_ms: sample.timestamp_ms(),
            value: sample.rates(),
        });
        true
    }

    pub(super) fn sparkline(&self, metric: usize, generation: u64) -> Vec<(SharedString, f64)> {
        if self.generation != generation {
            return Vec::new();
        }
        self.samples
            .iter()
            .map(|sample| {
                (
                    sample.at_ms.to_string().into(),
                    if metric == 0 {
                        sample.value.download as f64
                    } else {
                        sample.value.upload as f64
                    },
                )
            })
            .collect()
    }

    fn displayed(&self) -> impl Iterator<Item = &TimedSample> {
        let samples = &self.samples;
        let end = samples.back().map_or(0, |sample| sample.at_ms);
        let duration = HOME_CHART_WINDOW_SECONDS * 1_000;
        samples
            .iter()
            .filter(move |sample| sample.at_ms >= end.saturating_sub(duration))
    }

    pub(super) fn points(&self, generation: u64) -> (Vec<TrafficChartPoint>, u64) {
        if self.generation != generation {
            return (Vec::new(), 0);
        }
        let visible = self.displayed().collect::<Vec<_>>();
        let end = visible.last().map_or(0, |sample| sample.at_ms);
        let start = visible.first().map_or(end, |sample| sample.at_ms);
        let values = visible.iter().map(|sample| sample.value).collect();
        let ceiling = traffic_chart_ceiling(&values);
        let points = visible
            .into_iter()
            .map(|sample| TrafficChartPoint {
                label: if sample.at_ms == end {
                    zenclash_i18n::text("home.traffic.now").into()
                } else if end.saturating_sub(sample.at_ms) < 60_000 {
                    format!("−{}s", end.saturating_sub(sample.at_ms).div_ceil(1_000)).into()
                } else {
                    format!("−{:.1}m", end.saturating_sub(sample.at_ms) as f64 / 60_000.).into()
                },
                upload: chart_value(sample.value.upload),
                download: chart_value(sample.value.download),
                ceiling,
            })
            .collect();
        (points, end.saturating_sub(start) / 1_000)
    }
}

impl RuntimePage {
    pub(in crate::features) fn update_home_traffic_presentation(&mut self, cx: &mut Context<Self>) {
        if self.observe_home_traffic() {
            cx.notify();
        }
    }

    pub(super) fn observe_home_traffic(&mut self) -> bool {
        if self.page != Page::Home {
            return false;
        }
        let generation_changed = self.reconcile_home_generation();
        let traffic_changed = self
            .home
            .chart
            .observe_monitor(&self.monitors.traffic_monitor, self.home.generation);
        generation_changed || traffic_changed
    }
}

#[cfg(test)]
mod tests;
