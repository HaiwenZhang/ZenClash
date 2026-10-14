//! Reads a complete historical query from one `SQLite` snapshot.

use rusqlite::{Connection, params};

use super::to_sql_integer;
use crate::traffic_history::{
    TrafficAggregate, TrafficDimension, TrafficHistoryError, TrafficHistoryQuery,
    TrafficHistoryResult, TrafficHistorySelection, TrafficHistorySnapshot, TrafficOverview,
    TrafficTotals, TrafficTrendPoint,
};

const MAX_RANKINGS: u32 = 200;

pub(super) fn snapshot(
    connection: &mut Connection,
    query: &TrafficHistoryQuery,
    selection: &TrafficHistorySelection,
) -> TrafficHistoryResult<TrafficHistorySnapshot> {
    let start = to_sql_integer(query.start_ms, "查询开始时间")?;
    let end = to_sql_integer(query.end_ms, "查询结束时间")?;
    let bucket = to_sql_integer(query.bucket_ms, "趋势分桶")?;
    // A deferred read transaction pins the WAL snapshot at the first SELECT.
    // Retention and recording can continue without mixing their commits here.
    let transaction = connection.transaction()?;
    let totals = read_totals(&transaction, start, end)?;
    let rankings = read_rankings(&transaction, query.dimension, start, end)?;
    let trend = read_trend(&transaction, query, start, end, bucket)?;
    let (details, proxy_stats) = match selection {
        TrafficHistorySelection::Overview => (Vec::new(), Vec::new()),
        TrafficHistorySelection::Parent { parent } => {
            (self::details(&transaction, query, parent)?, Vec::new())
        }
        TrafficHistorySelection::Detail { parent, detail } => (
            self::details(&transaction, query, parent)?,
            self::proxy_stats(&transaction, query, parent, detail)?,
        ),
    };
    transaction.commit()?;
    Ok(TrafficHistorySnapshot {
        overview: TrafficOverview {
            rankings,
            trend,
            totals,
        },
        details,
        proxy_stats,
    })
}

pub(super) fn details(
    connection: &Connection,
    query: &TrafficHistoryQuery,
    parent: &str,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let start = to_sql_integer(query.start_ms, "查询开始时间")?;
    let end = to_sql_integer(query.end_ms, "查询结束时间")?;
    read_filtered_rankings(
        connection,
        query.dimension.column(),
        drill_down_column(query.dimension),
        parent,
        start,
        end,
    )
}

pub(super) fn proxy_stats(
    connection: &Connection,
    query: &TrafficHistoryQuery,
    parent: &str,
    detail: &str,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let start = to_sql_integer(query.start_ms, "查询开始时间")?;
    let end = to_sql_integer(query.end_ms, "查询结束时间")?;
    read_proxy_stats(connection, query.dimension, parent, detail, start, end)
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
    dimension: TrafficDimension,
    parent: &str,
    detail: &str,
    start: i64,
    end: i64,
) -> TrafficHistoryResult<Vec<TrafficAggregate>> {
    let secondary = drill_down_column(dimension);
    let primary = dimension.column();
    let sql = format!(
        "SELECT outbound, SUM(upload), SUM(download), COUNT(*)
         FROM traffic_history
         WHERE {primary} = ?1 AND {secondary} = ?2 AND timestamp_ms BETWEEN ?3 AND ?4
         GROUP BY outbound
         ORDER BY SUM(upload) + SUM(download) DESC, outbound ASC LIMIT ?5"
    );
    read_aggregates(
        connection,
        &sql,
        params![parent, detail, start, end, MAX_RANKINGS],
    )
}

fn drill_down_column(dimension: TrafficDimension) -> &'static str {
    match dimension {
        TrafficDimension::Host => "source_ip",
        TrafficDimension::SourceIp | TrafficDimension::Outbound | TrafficDimension::Process => {
            "host"
        }
    }
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

fn from_sql_integer(value: i64, label: &str) -> TrafficHistoryResult<u64> {
    u64::try_from(value).map_err(|_| TrafficHistoryError::ValueOutOfRange(label.to_owned()))
}
