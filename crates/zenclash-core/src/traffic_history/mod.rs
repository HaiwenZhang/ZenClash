//! Persistent traffic accounting derived from real Mihomo connection deltas.

use thiserror::Error;

mod logger;
mod model;
mod storage;

#[cfg(test)]
mod tests;

pub use logger::TrafficDeltaLogger;
pub use model::{
    DEFAULT_TRAFFIC_RETENTION_DAYS, TrafficAggregate, TrafficDimension, TrafficHistoryEntry,
    TrafficHistoryQuery, TrafficOverview, TrafficTotals, TrafficTrendPoint,
};
pub use storage::TrafficHistoryStore;

/// Errors produced by traffic-history validation and `SQLite` persistence.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TrafficHistoryError {
    /// A platform application-data directory could not be found.
    #[error("无法确定流量历史数据目录")]
    MissingDataDirectory,
    /// Application-preference discovery failed before locating the database.
    #[error("无法定位流量历史数据库：{0}")]
    Preferences(#[from] crate::AppPreferencesError),
    /// Filesystem access failed.
    #[error("流量历史 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// `SQLite` rejected a schema, query, or transaction operation.
    #[error("流量历史数据库错误：{0}")]
    Sql(#[from] rusqlite::Error),
    /// A query range or bucket was invalid or too large.
    #[error("流量历史查询无效：{0}")]
    InvalidQuery(String),
    /// An unsigned application value could not be represented by `SQLite`.
    #[error("流量历史数值超出 SQLite 范围：{0}")]
    ValueOutOfRange(String),
}

/// Result type returned by traffic-history operations.
pub type TrafficHistoryResult<T> = Result<T, TrafficHistoryError>;
