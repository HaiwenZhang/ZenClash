use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, Transaction, params};

use super::{
    TrafficAggregate, TrafficHistoryEntry, TrafficHistoryError, TrafficHistoryQuery,
    TrafficHistoryResult, TrafficHistorySelection, TrafficHistorySnapshot, TrafficOverview,
};
use crate::AppPreferencesStore;
use crate::data_coordinator::{DataWriteAccess, DataWriteLease};

mod query;

const MAX_INSERT_BATCH: usize = 10_000;
const MAX_HISTORY_ENTRIES: i64 = 1_000_000;
/// Cloneable handle to `ZenClash`'s native `SQLite` traffic database.
#[derive(Clone, Debug)]
pub struct TrafficHistoryStore {
    path: PathBuf,
    write_access: DataWriteAccess,
}

struct HistoryConnection {
    // Closing a WAL connection may write; release the lease afterwards.
    connection: Connection,
    _lease: DataWriteLease,
}

impl std::ops::Deref for HistoryConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}

impl std::ops::DerefMut for HistoryConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

impl TrafficHistoryStore {
    /// Opens the platform-default traffic database alongside application preferences.
    ///
    /// # Errors
    ///
    /// Returns an error when the application-data directory cannot be determined.
    pub fn discover() -> TrafficHistoryResult<Self> {
        let preferences = AppPreferencesStore::discover().map_err(|error| match error {
            crate::AppPreferencesError::MissingDataDirectory => {
                TrafficHistoryError::MissingDataDirectory
            }
            other => TrafficHistoryError::Preferences(other),
        })?;
        let directory = preferences
            .path()
            .parent()
            .ok_or(TrafficHistoryError::MissingDataDirectory)?;
        Ok(Self::new(directory.join("traffic-history.sqlite3")))
    }

    /// Creates a store backed by an explicit `SQLite` file.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let write_access = DataWriteAccess::new(&path);
        Self { path, write_access }
    }

    /// Returns the database file used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persists a bounded batch and applies time and total-row retention atomically.
    ///
    /// At most one million samples are retained. The oldest observation times
    /// are evicted first, with insertion order breaking equal-timestamp ties.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized batches, out-of-range values, filesystem failures,
    /// or `SQLite` transaction failures.
    pub fn insert_and_cleanup(
        &self,
        entries: &[TrafficHistoryEntry],
        cutoff_ms: u64,
    ) -> TrafficHistoryResult<()> {
        if entries.len() > MAX_INSERT_BATCH {
            return Err(TrafficHistoryError::InvalidQuery(format!(
                "单次最多写入 {MAX_INSERT_BATCH} 条流量记录"
            )));
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        insert_entries(&transaction, entries)?;
        transaction.execute(
            "DELETE FROM traffic_history WHERE timestamp_ms < ?1",
            [to_sql_integer(cutoff_ms, "清理时间")?],
        )?;
        transaction.execute(
            "DELETE FROM traffic_history WHERE id IN (
                 SELECT id FROM traffic_history
                 ORDER BY timestamp_ms ASC, id ASC
                 LIMIT MAX(0, (SELECT COUNT(*) FROM traffic_history) - ?1)
             )",
            [MAX_HISTORY_ENTRIES],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads an overview and selected drill-down results from one database snapshot.
    ///
    /// Recording, cleanup and clear operations cannot change the database view
    /// between the totals, rankings, trend, details and proxy-statistics reads.
    /// Each rankings list remains bounded to 200 entries.
    ///
    /// # Errors
    /// Returns an error for an invalid query, filesystem access, or `SQLite` failure.
    pub fn query(
        &self,
        query: &TrafficHistoryQuery,
        selection: &TrafficHistorySelection,
    ) -> TrafficHistoryResult<TrafficHistorySnapshot> {
        query.validate()?;
        let mut connection = self.open()?;
        query::snapshot(&mut connection, query, selection)
    }

    /// Loads totals, rankings and trend from one database snapshot.
    ///
    /// # Errors
    /// Returns an error when the query is invalid or `SQLite` cannot read the database.
    pub fn overview(&self, query: &TrafficHistoryQuery) -> TrafficHistoryResult<TrafficOverview> {
        self.query(query, &TrafficHistorySelection::Overview)
            .map(|snapshot| snapshot.overview)
    }

    /// Loads the next drill-down level for a ranking label.
    ///
    /// Host rankings drill down to source devices. All other dimensions drill down to hosts.
    /// Use [`Self::query`] when combining this result with an overview or proxy statistics.
    ///
    /// # Errors
    ///
    /// Returns an error when the query is invalid or `SQLite` cannot read the database.
    pub fn details(
        &self,
        query: &TrafficHistoryQuery,
        parent: &str,
    ) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
        query.validate()?;
        let connection = self.open()?;
        query::details(&connection, query, parent)
    }

    /// Loads outbound proxy usage for a selected ranking and drill-down label.
    /// Use [`Self::query`] when combining this result with other historical facts.
    ///
    /// # Errors
    ///
    /// Returns an error when the query is invalid or `SQLite` cannot read the database.
    pub fn proxy_stats(
        &self,
        query: &TrafficHistoryQuery,
        parent: &str,
        detail: &str,
    ) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
        query.validate()?;
        let connection = self.open()?;
        query::proxy_stats(&connection, query, parent, detail)
    }

    /// Deletes all persisted traffic records without deleting the database schema.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot clear the table.
    pub fn clear(&self) -> TrafficHistoryResult<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM traffic_history", [])?;
        transaction.execute(
            "INSERT INTO traffic_history_state (id, cleared_before_ms)
             VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET cleared_before_ms = MAX(cleared_before_ms, excluded.cleared_before_ms)",
            [to_sql_integer(unix_millis(), "清空时间")?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn open(&self) -> TrafficHistoryResult<HistoryConnection> {
        let lease = self.write_access.acquire();
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS traffic_history (
                 id INTEGER PRIMARY KEY,
                 timestamp_ms INTEGER NOT NULL CHECK(timestamp_ms >= 0),
                 source_ip TEXT NOT NULL,
                 host TEXT NOT NULL,
                 outbound TEXT NOT NULL,
                 process TEXT NOT NULL,
                 upload INTEGER NOT NULL CHECK(upload >= 0),
                 download INTEGER NOT NULL CHECK(download >= 0)
             );
             CREATE TABLE IF NOT EXISTS traffic_history_state (
                 id INTEGER PRIMARY KEY CHECK(id = 1),
                 cleared_before_ms INTEGER NOT NULL CHECK(cleared_before_ms >= 0)
             );
             CREATE INDEX IF NOT EXISTS traffic_history_timestamp
                 ON traffic_history(timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_host
                 ON traffic_history(host, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_source_ip
                 ON traffic_history(source_ip, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_outbound
                 ON traffic_history(outbound, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_process
                 ON traffic_history(process, timestamp_ms);",
        )?;
        Ok(HistoryConnection {
            connection,
            _lease: lease,
        })
    }
}

fn insert_entries(
    transaction: &Transaction<'_>,
    entries: &[TrafficHistoryEntry],
) -> TrafficHistoryResult<()> {
    let mut statement = transaction.prepare_cached(
        "INSERT INTO traffic_history (
             timestamp_ms, source_ip, host, outbound, process, upload, download
         ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
           WHERE ?1 > COALESCE(
               (SELECT cleared_before_ms FROM traffic_history_state WHERE id = 1), -1
           )",
    )?;
    for entry in entries {
        statement.execute(params![
            to_sql_integer(entry.timestamp_ms, "记录时间")?,
            entry.source_ip,
            entry.host,
            entry.outbound,
            entry.process,
            to_sql_integer(entry.upload, "上传字节")?,
            to_sql_integer(entry.download, "下载字节")?,
        ])?;
    }
    Ok(())
}

fn to_sql_integer(value: u64, label: &str) -> TrafficHistoryResult<i64> {
    i64::try_from(value).map_err(|_| TrafficHistoryError::ValueOutOfRange(label.to_owned()))
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
