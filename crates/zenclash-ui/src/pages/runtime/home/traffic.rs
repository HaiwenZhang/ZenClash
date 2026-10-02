use super::*;
use zenclash_core::TrafficSnapshot;

const HOME_CHART_SAMPLE_LIMIT: usize = 300;

#[derive(Clone, Copy)]
struct TimedSample {
    at_ms: u64,
    value: TrafficSample,
}

#[derive(Clone, Copy)]
struct ConnectionSample {
    at_ms: u64,
    count: usize,
    memory: u64,
}

#[derive(Default)]
pub(super) struct HomeChartState {
    connection_samples: VecDeque<ConnectionSample>,
    generation: u64,
    samples: VecDeque<TimedSample>,
    paused: Option<VecDeque<TimedSample>>,
    five_minutes: bool,
}

impl HomeChartState {
    fn observe(&mut self, snapshot: &TrafficSnapshot, generation: u64) -> bool {
        let generation_changed = self.generation != generation;
        if self.generation != generation {
            self.generation = generation;
            self.samples.clear();
            self.connection_samples.clear();
            self.paused = None;
        }
        if snapshot.generation != generation || snapshot.updated_at_ms == 0 {
            return generation_changed;
        }
        if self
            .samples
            .back()
            .is_some_and(|sample| sample.at_ms >= snapshot.updated_at_ms)
        {
            return generation_changed;
        }
        if self.samples.len() == HOME_CHART_SAMPLE_LIMIT {
            self.samples.pop_front();
        }
        self.samples.push_back(TimedSample {
            at_ms: snapshot.updated_at_ms,
            value: TrafficSample {
                upload: snapshot.upload,
                download: snapshot.download,
            },
        });
        true
    }

    fn observe_connections(&mut self, status: Option<&StreamStatus>) -> bool {
        let Some(status) = status
            .filter(|status| status.generation == self.generation && status.last_success_at_ms > 0)
        else {
            return false;
        };
        if self
            .connection_samples
            .back()
            .is_some_and(|sample| sample.at_ms >= status.last_success_at_ms)
        {
            return false;
        }
        if self.connection_samples.len() == HOME_CHART_SAMPLE_LIMIT {
            self.connection_samples.pop_front();
        }
        self.connection_samples.push_back(ConnectionSample {
            at_ms: status.last_success_at_ms,
            count: status.item_count,
            memory: status.memory,
        });
        true
    }

    pub(super) fn sparkline(&self, metric: usize, generation: u64) -> Vec<(SharedString, f64)> {
        if self.generation != generation {
            return Vec::new();
        }
        if metric < 2 {
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
        } else {
            self.connection_samples
                .iter()
                .map(|sample| {
                    (
                        sample.at_ms.to_string().into(),
                        if metric == 2 {
                            sample.count as f64
                        } else {
                            sample.memory as f64
                        },
                    )
                })
                .collect()
        }
    }

    fn displayed(&self) -> impl Iterator<Item = &TimedSample> {
        let samples = self.paused.as_ref().unwrap_or(&self.samples);
        let end = samples.back().map_or(0, |sample| sample.at_ms);
        let duration = if self.five_minutes { 300_000 } else { 60_000 };
        samples
            .iter()
            .filter(move |sample| sample.at_ms >= end.saturating_sub(duration))
    }

    fn toggle_pause(&mut self) {
        self.paused = if self.paused.is_some() {
            None
        } else {
            Some(self.samples.clone())
        };
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
                } else {
                    format!("−{:.1}s", end.saturating_sub(sample.at_ms) as f64 / 1_000.).into()
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
    pub(in crate::pages::runtime) fn update_home_traffic_presentation(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if self.observe_home_traffic() {
            cx.notify();
        }
        self.update_home_flow(cx);
        self.update_home_history(cx);
    }

    pub(super) fn observe_home_traffic(&mut self) -> bool {
        if self.page != Page::Home {
            return false;
        }
        let generation_changed = self.reconcile_home_generation();
        let traffic_changed = self
            .home
            .chart
            .observe(&self.traffic_monitor.snapshot(), self.home.generation);
        let connections_changed = self.home.chart.observe_connections(
            self.operational_status
                .snapshot()
                .streams
                .connections
                .value(),
        );
        generation_changed || traffic_changed || connections_changed
    }

    pub(super) fn render_home_chart_controls(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        h_flex()
            .gap_2()
            .child(
                Button::new("home-chart-60s")
                    .small()
                    .outline()
                    .label(zenclash_i18n::text("home.traffic.range_60"))
                    .selected(!self.home.chart.five_minutes)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.home.chart.five_minutes = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("home-chart-5m")
                    .small()
                    .outline()
                    .label(zenclash_i18n::text("home.traffic.range_300"))
                    .selected(self.home.chart.five_minutes)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.home.chart.five_minutes = true;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("home-chart-pause")
                    .small()
                    .outline()
                    .label(zenclash_i18n::text(if self.home.chart.paused.is_some() {
                        "home.traffic.resume"
                    } else {
                        "home.traffic.pause"
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.home.chart.toggle_pause();
                        cx.notify();
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(time: u64) -> TrafficSnapshot {
        TrafficSnapshot {
            generation: 1,
            updated_at_ms: time,
            upload: time,
            download: time * 2,
            ..TrafficSnapshot::default()
        }
    }
    #[test]
    fn metric_history_accepts_only_new_real_observations_and_releases_old_generation() {
        let mut state = HomeChartState::default();
        state.observe(&frame(1_000), 1);
        let status = StreamStatus {
            generation: 1,
            last_success_at_ms: 1_000,
            item_count: 7,
            memory: 4_096,
            ..StreamStatus::default()
        };
        state.observe_connections(Some(&status));
        state.observe_connections(Some(&status));
        assert_eq!(state.sparkline(2, 1).len(), 1);
        assert_eq!(state.sparkline(2, 1)[0].1, 7.);
        assert_eq!(state.sparkline(3, 1)[0].1, 4_096.);
        state.observe_connections(Some(&StreamStatus {
            generation: 2,
            last_success_at_ms: 2_000,
            ..status
        }));
        assert_eq!(state.sparkline(2, 1).len(), 1);
        state.observe(&frame(3_000), 2);
        assert!(state.sparkline(2, 2).is_empty());
    }

    #[test]
    fn chart_windows_use_actual_timestamps_and_do_not_duplicate_frames() {
        let mut state = HomeChartState::default();
        for seconds in 1..=400 {
            state.observe(&frame(seconds * 1_000), 1);
        }
        state.observe(&frame(400_000), 1);
        assert_eq!(state.samples.len(), 300);
        assert_eq!(state.points(1).1, 60);
        assert_eq!(state.points(1).0.len(), 61);
        state.five_minutes = true;
        assert_eq!(state.points(1).0.len(), 300);
        assert_eq!(state.points(1).1, 299);
    }
    #[test]
    fn pausing_freezes_only_the_display_and_generation_change_clears_old_samples() {
        let mut state = HomeChartState::default();
        state.observe(&frame(1_000), 1);
        state.toggle_pause();
        state.observe(&frame(2_000), 1);
        assert_eq!(state.points(1).0[0].upload, 1_000.);
        assert_eq!(state.samples.len(), 2);
        state.toggle_pause();
        assert_eq!(state.points(1).0.len(), 2);
        state.observe(&frame(3_000), 2);
        assert!(state.samples.is_empty());
        assert!(state.paused.is_none());
        assert!(state.points(1).0.is_empty());
    }
}
