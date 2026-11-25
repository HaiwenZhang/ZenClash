use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::{
    TrafficAggregate, TrafficHistoryEntry, TrafficHistoryError, TrafficHistoryQuery,
    TrafficHistoryResult, TrafficHistorySelection, TrafficHistorySnapshot, TrafficOverview,
};
use crate::AppPreferencesStore;
use crate::platform::data_coordinator::DataWriteAccess;
#[cfg(test)]
use crate::platform::data_coordinator::DataWriteLease;

#[cfg(test)]
mod permissions_tests;
mod private_io;
mod query;

const MAX_INSERT_BATCH: usize = 10_000;
const MAX_HISTORY_ENTRIES: i64 = 1_000_000;
#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(not(unix))]
type FileIdentity = ();

struct CachedConnection {
    connection: Connection,
    file_id: Option<FileIdentity>,
}

fn current_file_identity(path: &Path) -> std::io::Result<Option<FileIdentity>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Ok(Some((metadata.dev(), metadata.ino())))
            }
            #[cfg(not(unix))]
            {
                Ok(Some(()))
            }
        }
        Ok(_) => Err(std::io::Error::other(
            "traffic history path is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Cloneable handle to `ZenClash`'s native `SQLite` traffic database.
#[derive(Clone)]
pub struct TrafficHistoryStore {
    path: PathBuf,
    write_access: DataWriteAccess,
    connection: Arc<Mutex<Option<CachedConnection>>>,
}

impl fmt::Debug for TrafficHistoryStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrafficHistoryStore")
            .field("path", &self.path)
            .field("connected", &self.connection.lock().is_some())
            .finish()
    }
}

#[cfg(test)]
struct HistoryConnection {
    // Closing a WAL connection may write; release the lease afterwards.
    connection: Connection,
    _lease: DataWriteLease,
}

#[cfg(test)]
impl std::ops::Deref for HistoryConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}

#[cfg(test)]
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
        Self {
            path,
            write_access,
            connection: Arc::new(Mutex::new(None)),
        }
    }

    /// Explicitly closes any cached persistent `SQLite` connection so WAL/SHM
    /// file handles are released before backup restore or file replacement.
    pub fn close_connection(&self) {
        drop(self.connection.lock().take());
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
        let cutoff = to_sql_integer(cutoff_ms, "清理时间")?;
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            insert_entries(&transaction, entries)?;
            transaction.execute(
                "DELETE FROM traffic_history WHERE timestamp_ms < ?1",
                [cutoff],
            )?;
            evict_over_budget(&transaction, MAX_HISTORY_ENTRIES)?;
            transaction.commit()?;
            Ok(())
        })
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
        self.with_connection(|connection| query::snapshot(connection, query, selection))
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
        self.with_connection(|connection| query::details(connection, query, parent))
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
        self.with_connection(|connection| query::proxy_stats(connection, query, parent, detail))
    }

    /// Deletes all persisted traffic records without deleting the database schema.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot clear the table.
    pub fn clear(&self) -> TrafficHistoryResult<()> {
        let cleared_before = to_sql_integer(unix_millis(), "清空时间")?;
        let result = self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM traffic_history", [])?;
            transaction.execute(
                "INSERT INTO traffic_history_state (id, cleared_before_ms)
                 VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET cleared_before_ms = MAX(cleared_before_ms, excluded.cleared_before_ms)",
                [cleared_before],
            )?;
            transaction.commit()?;
            Ok(())
        });
        // Release the cached handle after clear so subsequent restore/replacement never sees a held handle.
        self.close_connection();
        result
    }

    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> TrafficHistoryResult<T>,
    ) -> TrafficHistoryResult<T> {
        let _lease = self.write_access.acquire()?;
        let mut guard = self.connection.lock();
        let disk_id = match current_file_identity(&self.path) {
            Ok(id) => id,
            Err(error) => {
                *guard = None;
                return Err(TrafficHistoryError::Io(error));
            }
        };
        let cache_valid = guard
            .as_ref()
            .is_some_and(|cached| disk_id.is_some() && cached.file_id == disk_id);
        if !cache_valid {
            *guard = None;
            let connection = Self::open_initialized(&self.path)?;
            let opened_id = current_file_identity(&self.path)?;
            *guard = Some(CachedConnection {
                connection,
                file_id: opened_id,
            });
        }
        let Some(cached) = guard.as_mut() else {
            return Err(TrafficHistoryError::Io(std::io::Error::other(
                "SQLite connection state unavailable",
            )));
        };
        let result = operation(&mut cached.connection);
        if result.is_err() {
            // Drop potentially wedged/corrupted handles so the next operation reopens cleanly.
            *guard = None;
        }
        result
    }

    #[cfg(test)]
    fn open(&self) -> TrafficHistoryResult<HistoryConnection> {
        let lease = self.write_access.acquire()?;
        let connection = Self::open_initialized(&self.path)?;
        Ok(HistoryConnection {
            connection,
            _lease: lease,
        })
    }

    fn open_initialized(path: &Path) -> TrafficHistoryResult<Connection> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        private_io::prepare(path)?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        // SQLite can reopen sidecars while entering WAL mode; protect them before writing data.
        #[cfg(windows)]
        private_io::prepare(path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS traffic_history (
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
             CREATE INDEX IF NOT EXISTS traffic_history_timestamp_totals
                 ON traffic_history(timestamp_ms, upload, download);
             CREATE INDEX IF NOT EXISTS traffic_history_host
                 ON traffic_history(host, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_source_ip
                 ON traffic_history(source_ip, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_outbound
                 ON traffic_history(outbound, timestamp_ms);
             CREATE INDEX IF NOT EXISTS traffic_history_process
                 ON traffic_history(process, timestamp_ms);",
        )?;
        Ok(connection)
    }
}

fn evict_over_budget(transaction: &Transaction<'_>, budget: i64) -> TrafficHistoryResult<()> {
    let cutoff: Option<(i64, i64)> = {
        let mut statement = transaction.prepare_cached(
            "SELECT timestamp_ms, id
             FROM traffic_history
             ORDER BY timestamp_ms DESC, id DESC
             LIMIT 1 OFFSET ?1",
        )?;
        statement
            .query_row([budget], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?
    };
    if let Some((cutoff_ts, cutoff_id)) = cutoff {
        transaction.execute(
            "DELETE FROM traffic_history
             WHERE timestamp_ms < ?1
                OR (timestamp_ms = ?1 AND id <= ?2)",
            params![cutoff_ts, cutoff_id],
        )?;
    }
    Ok(())
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

#[cfg(test)]
mod connection_tests {
    use super::*;

    #[test]
    fn persistent_connection_is_reused_and_released_on_clear_and_close() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-history-reuse-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let path = root.join("traffic-history.sqlite3");
        let store = TrafficHistoryStore::new(&path);
        assert!(store.connection.lock().is_none());

        store
            .insert_and_cleanup(
                &[TrafficHistoryEntry {
                    timestamp_ms: 1_000,
                    source_ip: "127.0.0.1".into(),
                    host: "example.com".into(),
                    outbound: "DIRECT".into(),
                    process: "curl".into(),
                    upload: 10,
                    download: 20,
                }],
                0,
            )
            .unwrap();
        assert!(store.connection.lock().is_some());

        // Covering index must exist on the initialized database.
        let index_exists: i64 = store
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'traffic_history_timestamp_totals'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(index_exists, 1);

        store.close_connection();
        assert!(store.connection.lock().is_none());

        store.clear().unwrap();
        assert!(store.connection.lock().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn offset_cutoff_eviction_keeps_newest_entries_with_id_tiebreak() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE traffic_history (
                 id INTEGER PRIMARY KEY,
                 timestamp_ms INTEGER NOT NULL,
                 source_ip TEXT NOT NULL,
                 host TEXT NOT NULL,
                 outbound TEXT NOT NULL,
                 process TEXT NOT NULL,
                 upload INTEGER NOT NULL,
                 download INTEGER NOT NULL
             );",
        )
        .unwrap();
        for (ts, host) in [
            (100_i64, "old-1"),
            (200, "tie-older-id"),
            (200, "tie-newer-id"),
            (300, "newest"),
        ] {
            conn.execute(
                "INSERT INTO traffic_history (timestamp_ms, source_ip, host, outbound, process, upload, download)
                 VALUES (?1, '127.0.0.1', ?2, 'DIRECT', 'app', 1, 1)",
                params![ts, host],
            )
            .unwrap();
        }
        let tx = conn.transaction().unwrap();
        evict_over_budget(&tx, 2).unwrap();
        tx.commit().unwrap();

        let mut stmt = conn
            .prepare("SELECT host FROM traffic_history ORDER BY timestamp_ms ASC, id ASC")
            .unwrap();
        let remaining: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(remaining, vec!["tie-newer-id", "newest"]);
    }
}
