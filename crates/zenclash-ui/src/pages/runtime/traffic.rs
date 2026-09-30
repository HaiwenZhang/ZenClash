use std::time::{SystemTime, UNIX_EPOCH};

use zenclash_core::{TrafficAggregate, TrafficDimension, TrafficOverview};

use super::loader::PageReadTask;

mod actions;
mod view;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TrafficRange {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl TrafficRange {
    const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Month];

    fn label(self) -> String {
        match self {
            Self::Hour => zenclash_i18n::text("traffic.range.hour"),
            Self::Day => zenclash_i18n::text("traffic.range.day"),
            Self::Week => zenclash_i18n::text("traffic.range.week"),
            Self::Month => zenclash_i18n::text("traffic.range.month"),
        }
    }

    const fn duration_ms(self) -> u64 {
        match self {
            Self::Hour => 60 * 60 * 1_000,
            Self::Day => 24 * 60 * 60 * 1_000,
            Self::Week => 7 * 24 * 60 * 60 * 1_000,
            Self::Month => 30 * 24 * 60 * 60 * 1_000,
        }
    }

    const fn bucket_ms(self) -> u64 {
        match self {
            Self::Hour => 5 * 60 * 1_000,
            Self::Day => 60 * 60 * 1_000,
            Self::Week => 6 * 60 * 60 * 1_000,
            Self::Month => 24 * 60 * 60 * 1_000,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct TrafficHistoryUiState {
    pub(super) range: TrafficRange,
    pub(super) dimension: TrafficDimension,
    pub(super) overview: TrafficOverview,
    pub(super) details: Vec<TrafficAggregate>,
    pub(super) proxy_stats: Vec<TrafficAggregate>,
    pub(super) selected_parent: Option<String>,
    pub(super) selected_detail: Option<String>,
    pub(super) loading: bool,
    pub(super) clear_confirmation: bool,
    pub(super) last_success_at_ms: Option<u64>,
    pub(super) last_error: Option<String>,
    revision: u64,
    request_generation: u64,
    task: PageReadTask,
    query_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Copy, Debug)]
struct HistoryRequest {
    generation: u64,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrafficHistoryFreshness {
    Loading,
    Fresh { observed_at_ms: u64 },
    Stale { observed_at_ms: u64 },
    Failed,
}

impl TrafficHistoryUiState {
    fn begin_query(&mut self) -> HistoryRequest {
        self.request_generation = self.request_generation.wrapping_add(1);
        self.loading = true;
        HistoryRequest {
            generation: self.request_generation,
            revision: self.revision,
        }
    }

    pub(super) fn cancel_query(&mut self) {
        self.task.cancel();
        self.request_generation = self.request_generation.wrapping_add(1);
        self.loading = false;
    }

    fn complete_query(
        &mut self,
        token: HistoryRequest,
        result: Result<TrafficHistoryPayload, String>,
        observed_at_ms: u64,
    ) -> bool {
        if token.generation != self.request_generation || !self.loading {
            return false;
        }
        self.loading = false;
        if token.revision != self.revision {
            return true;
        }
        match result {
            Ok(payload) => {
                self.overview = payload.overview;
                self.details = payload.details;
                self.proxy_stats = payload.proxy_stats;
                self.last_success_at_ms = Some(observed_at_ms);
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error),
        }
        false
    }

    fn freshness(&self) -> TrafficHistoryFreshness {
        match (self.last_success_at_ms, self.last_error.is_some()) {
            (Some(observed_at_ms), false) => TrafficHistoryFreshness::Fresh { observed_at_ms },
            (Some(observed_at_ms), true) => TrafficHistoryFreshness::Stale { observed_at_ms },
            (None, true) => TrafficHistoryFreshness::Failed,
            (None, false) => TrafficHistoryFreshness::Loading,
        }
    }

    pub(super) fn release_results(&mut self) {
        self.cancel_query();
        self.overview = TrafficOverview::default();
        self.details = Vec::new();
        self.proxy_stats = Vec::new();
        self.selected_parent = None;
        self.selected_detail = None;
        self.loading = false;
        self.clear_confirmation = false;
        self.last_success_at_ms = None;
        self.last_error = None;
        self.revision = self.revision.wrapping_add(1);
    }
}

#[derive(Debug)]
struct TrafficHistoryPayload {
    overview: TrafficOverview,
    details: Vec<TrafficAggregate>,
    proxy_stats: Vec<TrafficAggregate>,
}

fn dimension_label(dimension: TrafficDimension) -> String {
    match dimension {
        TrafficDimension::Host => zenclash_i18n::text("traffic.dimension.host"),
        TrafficDimension::SourceIp => zenclash_i18n::text("traffic.dimension.source"),
        TrafficDimension::Outbound => zenclash_i18n::text("traffic.dimension.outbound"),
        TrafficDimension::Process => zenclash_i18n::text("traffic.dimension.process"),
    }
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_completion_cannot_change_the_loading_or_error_of_a_reopened_page() {
        for result in [
            Ok(TrafficHistoryPayload {
                overview: TrafficOverview::default(),
                details: vec![TrafficAggregate::default()],
                proxy_stats: Vec::new(),
            }),
            Err("old query failed".into()),
        ] {
            let mut state = TrafficHistoryUiState::default();
            let old = state.begin_query();
            state.release_results();
            let current = state.begin_query();
            assert!(!state.complete_query(old, result, 1));
            assert!(state.loading);
            assert!(state.last_error.is_none());
            assert!(state.details.is_empty());
            state.complete_query(
                current,
                Ok(TrafficHistoryPayload {
                    overview: TrafficOverview::default(),
                    details: Vec::new(),
                    proxy_stats: Vec::new(),
                }),
                2,
            );
            assert!(!state.loading);
            assert_eq!(state.last_success_at_ms, Some(2));
        }
    }

    #[test]
    fn cancelling_a_hidden_query_prevents_publishing_or_requesting_another_query() {
        let mut state = TrafficHistoryUiState::default();
        let token = state.begin_query();
        state.revision += 1;
        state.cancel_query();
        assert!(!state.complete_query(token, Err("hidden query failed".into()), 1));
        assert!(!state.loading);
        assert!(state.last_error.is_none());
    }

    #[test]
    fn changing_query_selection_requests_one_refresh_without_publishing_old_data() {
        let mut state = TrafficHistoryUiState::default();
        let token = state.begin_query();
        state.revision += 1;
        assert!(state.complete_query(token, Err("obsolete selection failed".into()), 1));
        assert!(state.last_error.is_none());
        assert_eq!(state.last_success_at_ms, None);
        assert!(!state.complete_query(token, Err("duplicate completion".into()), 2));
    }

    #[test]
    fn history_ranges_produce_the_expected_bounded_bucket_counts() {
        let expected = [12_u64, 24, 28, 30];
        for (range, expected_count) in TrafficRange::ALL.into_iter().zip(expected) {
            let count = range.duration_ms().saturating_sub(1) / range.bucket_ms() + 1;
            assert_eq!(count, expected_count);
            assert!(count <= 512);
        }
    }

    #[test]
    fn every_traffic_dimension_has_a_distinct_user_label() {
        let labels = [
            dimension_label(TrafficDimension::Host),
            dimension_label(TrafficDimension::SourceIp),
            dimension_label(TrafficDimension::Outbound),
            dimension_label(TrafficDimension::Process),
        ];
        assert!(labels.iter().all(|label| !label.is_empty()));
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4
        );
    }

    #[test]
    fn historical_data_becomes_stale_instead_of_disappearing_after_failure() {
        let state = TrafficHistoryUiState {
            last_success_at_ms: Some(5_000),
            last_error: Some("database busy".into()),
            ..TrafficHistoryUiState::default()
        };

        assert_eq!(
            state.freshness(),
            TrafficHistoryFreshness::Stale {
                observed_at_ms: 5_000
            }
        );
    }

    #[test]
    fn leaving_traffic_releases_derived_results_but_keeps_the_user_selection() {
        let mut state = TrafficHistoryUiState {
            range: TrafficRange::Week,
            dimension: TrafficDimension::Outbound,
            details: vec![TrafficAggregate::default()],
            proxy_stats: vec![TrafficAggregate::default()],
            selected_parent: Some("Proxy".into()),
            selected_detail: Some("Node".into()),
            last_success_at_ms: Some(5_000),
            revision: 7,
            ..TrafficHistoryUiState::default()
        };

        state.release_results();

        assert_eq!(state.range, TrafficRange::Week);
        assert_eq!(state.dimension, TrafficDimension::Outbound);
        assert!(state.details.is_empty());
        assert!(state.proxy_stats.is_empty());
        assert_eq!(state.selected_parent, None);
        assert_eq!(state.selected_detail, None);
        assert_eq!(state.last_success_at_ms, None);
        assert_eq!(state.revision, 8);
    }
}
