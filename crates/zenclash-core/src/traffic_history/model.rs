/// Default number of days retained by the native traffic database.
pub const DEFAULT_TRAFFIC_RETENTION_DAYS: u16 = 30;

const MAX_BUCKETS: u64 = 512;
const MAX_QUERY_RANGE_MS: u64 = 366 * 24 * 60 * 60 * 1_000;

/// One positive traffic delta attributed to a Mihomo connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficHistoryEntry {
    /// Observation time in Unix milliseconds.
    pub timestamp_ms: u64,
    /// Source client address or `Inner` for local traffic.
    pub source_ip: String,
    /// Sniffed hostname, falling back to destination IP.
    pub host: String,
    /// First outbound in Mihomo's selected proxy chain.
    pub outbound: String,
    /// Owning process name when Mihomo can identify it.
    pub process: String,
    /// Bytes uploaded since the previous connection snapshot.
    pub upload: u64,
    /// Bytes downloaded since the previous connection snapshot.
    pub download: u64,
}

/// Dimension used to group historical traffic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrafficDimension {
    /// Group by sniffed host or destination address.
    #[default]
    Host,
    /// Group by source client address.
    SourceIp,
    /// Group by selected outbound proxy.
    Outbound,
    /// Group by owning process.
    Process,
}

impl TrafficDimension {
    pub(super) const fn column(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::SourceIp => "source_ip",
            Self::Outbound => "outbound",
            Self::Process => "process",
        }
    }
}

/// Validated time range and grouping parameters for an overview query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficHistoryQuery {
    /// Ranking dimension.
    pub dimension: TrafficDimension,
    /// Inclusive Unix-millisecond range start.
    pub start_ms: u64,
    /// Inclusive Unix-millisecond range end.
    pub end_ms: u64,
    /// Trend aggregation bucket width in milliseconds.
    pub bucket_ms: u64,
}

impl TrafficHistoryQuery {
    pub(super) fn validate(&self) -> super::TrafficHistoryResult<()> {
        if self.start_ms > self.end_ms {
            return Err(super::TrafficHistoryError::InvalidQuery(
                "开始时间晚于结束时间".into(),
            ));
        }
        let range = self.end_ms.saturating_sub(self.start_ms);
        if range > MAX_QUERY_RANGE_MS {
            return Err(super::TrafficHistoryError::InvalidQuery(
                "查询范围不能超过 366 天".into(),
            ));
        }
        if self.bucket_ms == 0 || range / self.bucket_ms + 1 > MAX_BUCKETS {
            return Err(super::TrafficHistoryError::InvalidQuery(format!(
                "趋势分桶必须大于 0 且不超过 {MAX_BUCKETS} 个"
            )));
        }
        Ok(())
    }
}

/// Aggregated traffic for one dimension label.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrafficAggregate {
    /// Dimension value.
    pub label: String,
    /// Uploaded bytes.
    pub upload: u64,
    /// Downloaded bytes.
    pub download: u64,
    /// Sum of uploaded and downloaded bytes.
    pub total: u64,
    /// Number of persisted delta samples.
    pub samples: u64,
}

/// One filled trend bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrafficTrendPoint {
    /// Aligned bucket start in Unix milliseconds.
    pub timestamp_ms: u64,
    /// Uploaded bytes in the bucket.
    pub upload: u64,
    /// Downloaded bytes in the bucket.
    pub download: u64,
}

/// Totals for the selected historical range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrafficTotals {
    /// Uploaded bytes.
    pub upload: u64,
    /// Downloaded bytes.
    pub download: u64,
    /// Sum of uploaded and downloaded bytes.
    pub total: u64,
    /// Number of persisted delta samples.
    pub samples: u64,
}

/// Rankings, filled trend buckets, and totals for a historical query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrafficOverview {
    /// Dimension rankings ordered by total bytes descending.
    pub rankings: Vec<TrafficAggregate>,
    /// Time buckets ordered chronologically, including empty buckets.
    pub trend: Vec<TrafficTrendPoint>,
    /// Aggregate totals across the query range.
    pub totals: TrafficTotals,
}
