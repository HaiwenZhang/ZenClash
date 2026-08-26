use std::{fmt, net::IpAddr, path::Path, str::FromStr};

use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::{WebDavError, WebDavResult};

pub(super) const DEFAULT_WEBDAV_DIRECTORY: &str = "zenclash";
const MAX_DIRECTORY_SEGMENTS: usize = 16;
const MAX_DIRECTORY_BYTES: usize = 512;
const MAX_BACKUPS: usize = 100;

/// Persistent connection and retention settings for a `WebDAV` server.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WebDavSettings {
    /// Schema version reserved for future migrations.
    pub version: u32,
    /// Base `WebDAV` endpoint, including any server-specific DAV path.
    pub url: String,
    /// Relative directory below the base endpoint used by `ZenClash`.
    pub directory: String,
    /// Optional HTTP Basic authentication username.
    pub username: String,
    /// Optional HTTP Basic authentication password.
    pub password: String,
    /// Maximum backups retained for this OS/device prefix; zero is unlimited.
    pub max_backups: usize,
    /// Whether explicitly user-approved invalid TLS certificates are accepted.
    pub accept_invalid_certificates: bool,
    /// Cron expression for background backups; empty disables scheduling.
    pub backup_cron: String,
}

impl Default for WebDavSettings {
    fn default() -> Self {
        Self {
            version: 1,
            url: String::new(),
            directory: DEFAULT_WEBDAV_DIRECTORY.into(),
            username: String::new(),
            password: String::new(),
            max_backups: 0,
            accept_invalid_certificates: false,
            backup_cron: String::new(),
        }
    }
}

impl fmt::Debug for WebDavSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebDavSettings")
            .field("version", &self.version)
            .field("url", &self.url)
            .field("directory", &self.directory)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("max_backups", &self.max_backups)
            .field(
                "accept_invalid_certificates",
                &self.accept_invalid_certificates,
            )
            .field("backup_cron", &self.backup_cron)
            .finish()
    }
}

impl WebDavSettings {
    pub(super) fn validate(&self) -> WebDavResult<ValidatedWebDavSettings> {
        if self.url.trim().is_empty() {
            return Err(WebDavError::InvalidSettings("请填写 WebDAV URL".into()));
        }
        let base_url = Url::parse(self.url.trim())
            .map_err(|error| WebDavError::InvalidSettings(format!("URL 解析失败：{error}")))?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(WebDavError::InvalidSettings(
                "URL 仅支持 http 或 https".into(),
            ));
        }
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(WebDavError::InvalidSettings(
                "URL 不能包含凭据、查询参数或片段".into(),
            ));
        }
        if base_url.cannot_be_a_base() {
            return Err(WebDavError::InvalidSettings("URL 不能作为目录基址".into()));
        }
        if base_url.scheme() == "http" && !self.username.is_empty() && !is_loopback_host(&base_url)
        {
            return Err(WebDavError::InvalidSettings(
                "非本机 Basic 登录必须使用 https，避免明文泄露凭据".into(),
            ));
        }
        let directory = validate_directory(&self.directory)?;
        if self.max_backups > MAX_BACKUPS {
            return Err(WebDavError::InvalidSettings(format!(
                "最大备份数不能超过 {MAX_BACKUPS}"
            )));
        }
        parse_backup_schedule(&self.backup_cron)?;
        Ok(ValidatedWebDavSettings {
            base_url,
            directory,
        })
    }

    /// Returns the next scheduled backup after a Unix timestamp.
    ///
    /// Five-field crontab expressions are accepted and normalized with a zero
    /// seconds field. Six- and seven-field expressions are passed through.
    /// An empty expression disables background backup and returns `None`.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed expressions, out-of-range timestamps, or
    /// schedules that have no future occurrence.
    pub fn next_backup_after(&self, unix_seconds: u64) -> WebDavResult<Option<u64>> {
        let Some(schedule) = parse_backup_schedule(&self.backup_cron)? else {
            return Ok(None);
        };
        let unix_seconds = i64::try_from(unix_seconds)
            .map_err(|error| WebDavError::Schedule(format!("时间戳超出范围：{error}")))?;
        let after_utc = DateTime::<Utc>::from_timestamp(unix_seconds, 0)
            .ok_or_else(|| WebDavError::Schedule("时间戳无法转换为日期".into()))?;
        let after_local = after_utc.with_timezone(&Local);
        let next = schedule
            .after(&after_local)
            .next()
            .ok_or_else(|| WebDavError::Schedule("计划没有下一次执行时间".into()))?;
        let next = u64::try_from(next.timestamp())
            .map_err(|error| WebDavError::Schedule(format!("下次执行时间超出范围：{error}")))?;
        Ok(Some(next))
    }
}

fn parse_backup_schedule(expression: &str) -> WebDavResult<Option<Schedule>> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Ok(None);
    }
    let field_count = expression.split_whitespace().count();
    let normalized = match field_count {
        5 => format!("0 {expression}"),
        6 | 7 => expression.to_owned(),
        _ => {
            return Err(WebDavError::Schedule(
                "Cron 必须包含 5、6 或 7 个字段".into(),
            ));
        }
    };
    Schedule::from_str(&normalized)
        .map(Some)
        .map_err(|error| WebDavError::Schedule(error.to_string()))
}

fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

pub(super) struct ValidatedWebDavSettings {
    pub(super) base_url: Url,
    pub(super) directory: Vec<String>,
}

/// Metadata for one ZIP archive returned by a `WebDAV` directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebDavBackup {
    /// Safe basename used for restore and deletion requests.
    pub filename: String,
    /// Server-reported archive size in bytes, when available.
    pub size_bytes: Option<u64>,
    /// Server-reported modification timestamp, when available.
    pub modified: Option<String>,
}

/// Result of uploading a local snapshot and enforcing retention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebDavUploadSummary {
    /// Newly uploaded remote backup.
    pub backup: WebDavBackup,
    /// Older same-device archives removed by the retention policy.
    pub removed_backups: usize,
}

pub(super) fn validate_filename(filename: &str) -> WebDavResult<()> {
    let path = Path::new(filename);
    let mut components = path.components();
    let single_component = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if filename.is_empty()
        || filename.len() > 255
        || !single_component
        || filename.contains(['/', '\\'])
        || filename.chars().any(char::is_control)
        || !filename
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(WebDavError::InvalidFilename(filename.into()));
    }
    Ok(())
}

fn validate_directory(directory: &str) -> WebDavResult<Vec<String>> {
    let directory = directory.trim().trim_matches('/');
    if directory.is_empty() || directory.len() > MAX_DIRECTORY_BYTES || directory.contains('\\') {
        return Err(WebDavError::InvalidSettings(
            "远程目录不能为空、过长或包含反斜杠".into(),
        ));
    }
    let segments = directory.split('/').collect::<Vec<_>>();
    if segments.len() > MAX_DIRECTORY_SEGMENTS
        || segments.iter().any(|segment| {
            segment.is_empty()
                || matches!(*segment, "." | "..")
                || segment.chars().any(char::is_control)
        })
    {
        return Err(WebDavError::InvalidSettings(
            "远程目录包含空白、越界或过多路径段".into(),
        ));
    }
    Ok(segments.into_iter().map(str::to_owned).collect())
}
