use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, Transaction, params};

use super::{
    TrafficAggregate, TrafficDimension, TrafficHistoryEntry, TrafficHistoryError,
    TrafficHistoryQuery, TrafficHistoryResult, TrafficOverview, TrafficTotals, TrafficTrendPoint,
};
use crate::AppPreferencesStore;

const MAX_INSERT_BATCH: usize = 10_000;
const MAX_RANKINGS: u32 = 200;

/// Cloneable handle to `ZenClash`'s native `SQLite` traffic database.
#[derive(Clone, Debug)]
pub struct TrafficHistoryStore {
    path: PathBuf,
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
        Self { path: path.into() }
    }

    /// Returns the database file used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persists a bounded batch and deletes records older than `cutoff_ms` atomically.
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
        transaction.commit()?;
        Ok(())
    }

    /// Loads totals, dimension rankings, and a gap-filled trend for one time range.
    ///
    /// # Errors
    ///
    /// Returns an error when the query is invalid or `SQLite` cannot read the database.
    pub fn overview(&self, query: &TrafficHistoryQuery) -> TrafficHistoryResult<TrafficOverview> {
        query.validate()?;
        let connection = self.open()?;
        let start = to_sql_integer(query.start_ms, "查询开始时间")?;
        let end = to_sql_integer(query.end_ms, "查询结束时间")?;
        let bucket = to_sql_integer(query.bucket_ms, "趋势分桶")?;
        let totals = read_totals(&connection, start, end)?;
        let rankings = read_rankings(&connection, query.dimension, start, end)?;
        let trend = read_trend(&connection, query, start, end, bucket)?;
        Ok(TrafficOverview {
            rankings,
            trend,
            totals,
        })
    }

    /// Loads the next drill-down level for a ranking label.
    ///
    /// Host rankings drill down to source devices. All other dimensions drill down to hosts.
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
        let start = to_sql_integer(query.start_ms, "查询开始时间")?;
        let end = to_sql_integer(query.end_ms, "查询结束时间")?;
        let (filter, group) = match query.dimension {
            TrafficDimension::Host => ("host", "source_ip"),
            TrafficDimension::SourceIp => ("source_ip", "host"),
            TrafficDimension::Outbound => ("outbound", "host"),
            TrafficDimension::Process => ("process", "host"),
        };
        read_filtered_rankings(&connection, filter, group, parent, start, end)
    }

    /// Loads outbound proxy usage for a selected ranking and drill-down label.
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
        let start = to_sql_integer(query.start_ms, "查询开始时间")?;
        let end = to_sql_integer(query.end_ms, "查询结束时间")?;
        read_proxy_stats(
            &connection,
            query.dimension.column(),
            parent,
            detail,
            start,
            end,
        )
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

    fn open(&self) -> TrafficHistoryResult<Connection> {
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
        Ok(connection)
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

fn read_totals(
    connection: &Connection,
    start: i64,
    end: i64,
) -> TrafficHistoryResult<TrafficTotals> {
    let (upload, download, samples) = connection.query_row(
        "SELECT COALESCE(SUM(upload), 0), COALESCE(SUM(download), 0), COUNT(*)
         FROM traffic_history WHERE timestamp_ms BETWEEN ?1 AND ?2",
        params![start, end],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    aggregate_totals(upload, download, samples)
}

fn read_rankings(
    connection: &Connection,
    dimension: TrafficDimension,
    start: i64,
    end: i64,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let sql = format!(
        "SELECT {column}, SUM(upload), SUM(download), COUNT(*)
         FROM traffic_history WHERE timestamp_ms BETWEEN ?1 AND ?2
         GROUP BY {column} ORDER BY SUM(upload) + SUM(download) DESC, {column} ASC LIMIT ?3",
        column = dimension.column()
    );
    read_aggregates(connection, &sql, params![start, end, MAX_RANKINGS])
}

fn read_filtered_rankings(
    connection: &Connection,
    filter: &str,
    group: &str,
    parent: &str,
    start: i64,
    end: i64,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let sql = format!(
        "SELECT {group}, SUM(upload), SUM(download), COUNT(*)
         FROM traffic_history
         WHERE {filter} = ?1 AND timestamp_ms BETWEEN ?2 AND ?3
         GROUP BY {group} ORDER BY SUM(upload) + SUM(download) DESC, {group} ASC LIMIT ?4"
    );
    read_aggregates(connection, &sql, params![parent, start, end, MAX_RANKINGS])
}

fn read_proxy_stats(
    connection: &Connection,
    dimension: &str,
    parent: &str,
    detail: &str,
    start: i64,
    end: i64,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let secondary = if dimension == "host" {
        "source_ip"
    } else {
        "host"
    };
    let sql = format!(
        "SELECT outbound, SUM(upload), SUM(download), COUNT(*)
         FROM traffic_history
         WHERE {dimension} = ?1 AND {secondary} = ?2 AND timestamp_ms BETWEEN ?3 AND ?4
         GROUP BY outbound
         ORDER BY SUM(upload) + SUM(download) DESC, outbound ASC LIMIT ?5"
    );
    read_aggregates(
        connection,
        &sql,
        params![parent, detail, start, end, MAX_RANKINGS],
    )
}

fn read_aggregates<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> TrafficHistoryResult<Vec<TrafficAggregate>>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(parameters, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (label, upload, download, samples) = row?;
        let totals = aggregate_totals(upload, download, samples)?;
        Ok(TrafficAggregate {
            label,
            upload: totals.upload,
            download: totals.download,
            total: totals.total,
            samples: totals.samples,
        })
    })
    .collect()
}

fn read_trend(
    connection: &Connection,
    query: &TrafficHistoryQuery,
    start: i64,
    end: i64,
    bucket: i64,
) -> TrafficHistoryResult<Vec<TrafficTrendPoint>> {
    let mut statement = connection.prepare(
        "SELECT (timestamp_ms - ?1) / ?3 AS bucket_index, SUM(upload), SUM(download)
         FROM traffic_history WHERE timestamp_ms BETWEEN ?1 AND ?2
         GROUP BY bucket_index ORDER BY bucket_index ASC",
    )?;
    let rows = statement.query_map(params![start, end, bucket], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let bucket_count = query.end_ms.saturating_sub(query.start_ms) / query.bucket_ms + 1;
    let mut trend = Vec::with_capacity(
        usize::try_from(bucket_count)
            .map_err(|_| TrafficHistoryError::InvalidQuery("趋势分桶数量超出平台范围".into()))?,
    );
    for index in 0..bucket_count {
        trend.push(TrafficTrendPoint {
            timestamp_ms: query
                .start_ms
                .saturating_add(index.saturating_mul(query.bucket_ms)),
            ..TrafficTrendPoint::default()
        });
    }
    for row in rows {
        let (index, upload, download) = row?;
        let index = usize::try_from(index)
            .map_err(|_| TrafficHistoryError::ValueOutOfRange("趋势分桶索引".into()))?;
        let point = trend.get_mut(index).ok_or_else(|| {
            TrafficHistoryError::InvalidQuery("SQLite 返回了范围外的趋势分桶".into())
        })?;
        point.upload = from_sql_integer(upload, "趋势上传字节")?;
        point.download = from_sql_integer(download, "趋势下载字节")?;
    }
    Ok(trend)
}

fn aggregate_totals(
    upload: i64,
    download: i64,
    samples: i64,
) -> TrafficHistoryResult<TrafficTotals> {
    let upload = from_sql_integer(upload, "上传字节")?;
    let download = from_sql_integer(download, "下载字节")?;
    let total = upload
        .checked_add(download)
        .ok_or_else(|| TrafficHistoryError::ValueOutOfRange("流量总字节溢出 u64".into()))?;
    Ok(TrafficTotals {
        upload,
        download,
        total,
        samples: from_sql_integer(samples, "样本数量")?,
    })
}

fn to_sql_integer(value: u64, label: &str) -> TrafficHistoryResult<i64> {
    i64::try_from(value).map_err(|_| TrafficHistoryError::ValueOutOfRange(label.to_owned()))
}

fn from_sql_integer(value: i64, label: &str) -> TrafficHistoryResult<u64> {
    u64::try_from(value).map_err(|_| TrafficHistoryError::ValueOutOfRange(label.to_owned()))
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
