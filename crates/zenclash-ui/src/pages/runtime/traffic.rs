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

    const fn label(self) -> &'static str {
        match self {
            Self::Hour => "1 小时",
            Self::Day => "24 小时",
            Self::Week => "7 天",
            Self::Month => "30 天",
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
    revision: u64,
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
        }
        Ok(_) => {}
        Err(error) => page.set_page_error(token, error),
    }
    false
}

const fn dimension_label(dimension: TrafficDimension) -> &'static str {
    match dimension {
        TrafficDimension::Host => "域名",
        TrafficDimension::SourceIp => "设备",
        TrafficDimension::Outbound => "出口",
        TrafficDimension::Process => "进程",
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
        assert_eq!(dimension_label(TrafficDimension::Host), "域名");
        assert_eq!(dimension_label(TrafficDimension::SourceIp), "设备");
        assert_eq!(dimension_label(TrafficDimension::Outbound), "出口");
        assert_eq!(dimension_label(TrafficDimension::Process), "进程");
    }
}
