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
    TrafficHistoryQuery, TrafficHistorySelection, TrafficHistorySnapshot, TrafficOverview,
    TrafficTotals, TrafficTrendPoint,
};
pub use storage::TrafficHistoryStore;

/// Errors produced by traffic-history validation and `SQLite` persistence.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TrafficHistoryError {
    /// A platform application-data directory could not be found.
    #[error(
        "{}",
        zenclash_i18n::text("settings.traffic_history.errors.missing_data_directory")
    )]
    MissingDataDirectory,
    /// Application-preference discovery failed before locating the database.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "settings.traffic_history.errors.preferences",
            &[("error", .0.to_string())]
        )
    )]
    Preferences(#[from] crate::AppPreferencesError),
    /// Filesystem access failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("settings.traffic_history.errors.io", &[("error", .0.to_string())])
    )]
    Io(#[from] std::io::Error),
    /// `SQLite` rejected a schema, query, or transaction operation.
    #[error(
        "{}",
        zenclash_i18n::text_with("settings.traffic_history.errors.sql", &[("error", .0.to_string())])
    )]
    Sql(#[from] rusqlite::Error),
    /// A query range or bucket was invalid or too large.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "settings.traffic_history.errors.invalid_query",
            &[("error", .0.clone())]
        )
    )]
    InvalidQuery(String),
    /// An unsigned application value could not be represented by `SQLite`.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "settings.traffic_history.errors.value_out_of_range",
            &[("error", .0.clone())]
        )
    )]
    ValueOutOfRange(String),
}

/// Result type returned by traffic-history operations.
pub type TrafficHistoryResult<T> = Result<T, TrafficHistoryError>;
