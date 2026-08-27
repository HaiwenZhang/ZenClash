use std::time::{SystemTime, UNIX_EPOCH};

use zenclash_core::{TrafficAggregate, TrafficDimension, TrafficOverview};

use super::{PageTaskToken, RuntimePage};

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

#[derive(Clone, Debug, Default)]
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrafficHistoryFreshness {
    Loading,
    Fresh { observed_at_ms: u64 },
    Stale { observed_at_ms: u64 },
    Failed,
}

impl TrafficHistoryUiState {
    fn freshness(&self) -> TrafficHistoryFreshness {
        match (self.last_success_at_ms, self.last_error.is_some()) {
            (Some(observed_at_ms), false) => TrafficHistoryFreshness::Fresh { observed_at_ms },
            (Some(observed_at_ms), true) => TrafficHistoryFreshness::Stale { observed_at_ms },
            (None, true) => TrafficHistoryFreshness::Failed,
            (None, false) => TrafficHistoryFreshness::Loading,
        }
    }
}

#[derive(Debug)]
struct TrafficHistoryPayload {
    overview: TrafficOverview,
    details: Vec<TrafficAggregate>,
    proxy_stats: Vec<TrafficAggregate>,
}

fn finish_history_refresh(
    page: &mut RuntimePage,
    token: PageTaskToken,
    revision: u64,
    result: Result<TrafficHistoryPayload, String>,
) -> bool {
    page.traffic_history.loading = false;
    if page.is_page_task_current(token) && page.traffic_history.revision != revision {
        return true;
    }
    match result {
        Ok(payload) if page.is_page_task_current(token) => {
            page.traffic_history.overview = payload.overview;
            page.traffic_history.details = payload.details;
            page.traffic_history.proxy_stats = payload.proxy_stats;
            page.traffic_history.last_success_at_ms = Some(unix_millis());
            page.traffic_history.last_error = None;
        }
        Ok(_) => {}
        Err(error) => {
            page.traffic_history.last_error = Some(error.clone());
            page.set_page_error(token, error);
        }
    }
    false
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
}
