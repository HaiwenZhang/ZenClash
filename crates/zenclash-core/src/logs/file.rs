//! Serialized, bounded persistence for the live Mihomo log stream.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    thread,
};

use parking_lot::RwLock;
use thiserror::Error;

use super::{format_log_entries, LogEntry};

const MEBIBYTE: u64 = 1024 * 1024;
const MIN_MAX_MEBIBYTES: u16 = 1;
const MAX_MAX_MEBIBYTES: u16 = 100;
const FILE_QUEUE_CAPACITY: usize = 512;
const COMPACTION_RETAIN_PERCENT: u64 = 50;
const TRUNCATE_MARKER: &[u8] =
    b"\n[ZENCLASH] Log compacted after reaching the configured size limit.\n";

/// Errors produced while configuring or writing continuous Mihomo logs.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LogPersistenceError {
    /// The configured limit is outside the supported range.
    #[error("日志文件大小上限必须在 1 到 100 MiB 之间")]
    InvalidLimit,
    /// Filesystem access failed.
    #[error("日志文件 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// Result type returned by persistent-log operations.
pub type LogPersistenceResult<T> = Result<T, LogPersistenceError>;

/// Current state of the continuous Mihomo log writer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogPersistenceStatus {
    /// Whether new entries are configured to be written.
    pub enabled: bool,
    /// Target file, when persistence has been configured.
    pub path: Option<PathBuf>,
    /// Configured maximum file size in bytes.
    pub max_bytes: u64,
    /// Last successfully observed file size.
    pub size_bytes: u64,
    /// Entries discarded because the disk queue was full.
    pub dropped_entries: u64,
    /// Most recent writer error, cleared after a successful write.
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct LogFileConfig {
    path: PathBuf,
    enabled: bool,
    max_bytes: u64,
}

impl LogFileConfig {
    fn from_mebibytes(
        path: PathBuf,
        enabled: bool,
        max_mebibytes: u16,
    ) -> LogPersistenceResult<Self> {
        if !(MIN_MAX_MEBIBYTES..=MAX_MAX_MEBIBYTES).contains(&max_mebibytes) {
            return Err(LogPersistenceError::InvalidLimit);
        }
        Ok(Self {
            path,
            enabled,
            max_bytes: u64::from(max_mebibytes) * MEBIBYTE,
        })
    }

    #[cfg(test)]
    fn from_bytes(path: PathBuf, enabled: bool, max_bytes: u64) -> Self {
        Self {
            path,
            enabled,
            max_bytes,
        }
    }
}

enum LogFileCommand {
    Append(LogEntry),
}

#[derive(Clone)]
pub(super) struct LogFileSender {
    sender: SyncSender<LogFileCommand>,
    settings: Arc<RwLock<Option<LogFileConfig>>>,
    status: Arc<RwLock<LogPersistenceStatus>>,
}

impl LogFileSender {
    pub(super) fn append(&self, entry: LogEntry) {
        if !self
            .settings
            .read()
            .as_ref()
            .is_some_and(|settings| settings.enabled)
        {
            return;
        }
        match self.sender.try_send(LogFileCommand::Append(entry)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let mut status = self.status.write();
                status.dropped_entries = status.dropped_entries.saturating_add(1);
                status.last_error = Some("日志磁盘队列已满；已丢弃一条落盘记录".into());
            }
            Err(TrySendError::Disconnected(_)) => {
                self.status.write().last_error = Some("日志文件写入线程已停止".into());
            }
        }
    }
}

pub(super) struct LogFileWorker {
    sender: LogFileSender,
    thread: Option<thread::JoinHandle<()>>,
}

impl LogFileWorker {
    pub(super) fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel(FILE_QUEUE_CAPACITY);
        let settings = Arc::new(RwLock::new(None));
        let status = Arc::new(RwLock::new(LogPersistenceStatus::default()));
        let file_sender = LogFileSender {
            sender,
            settings: settings.clone(),
            status: status.clone(),
        };
        let thread_settings = settings.clone();
        let thread_status = status.clone();
        let thread = thread::Builder::new()
            .name("zenclash-log-file".into())
            .spawn(move || {
                let mut writer = BoundedLogFile::default();
                while let Ok(LogFileCommand::Append(entry)) = receiver.recv() {
                    let config = thread_settings.read().clone();
                    let Some(config) = config.filter(|config| config.enabled) else {
                        continue;
                    };
                    match writer.append(&config, &entry) {
                        Ok(size) => {
                            let mut snapshot = thread_status.write();
                            snapshot.size_bytes = size;
                            snapshot.last_error = None;
                        }
                        Err(error) => thread_status.write().last_error = Some(error.to_string()),
                    }
                }
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                status.write().last_error = Some(format!("无法启动日志文件线程：{error}"));
                None
            }
        };
        Self {
            sender: file_sender,
            thread,
        }
    }

    pub(super) fn sender(&self) -> LogFileSender {
        self.sender.clone()
    }

    pub(super) fn configure(
        &self,
        path: PathBuf,
        enabled: bool,
        max_mebibytes: u16,
    ) -> LogPersistenceResult<()> {
        let config = LogFileConfig::from_mebibytes(path.clone(), enabled, max_mebibytes)?;
        *self.sender.settings.write() = Some(config.clone());
        let mut status = self.sender.status.write();
        status.enabled = enabled;
        status.path = Some(path);
        status.max_bytes = config.max_bytes;
        let observed_size = fs::metadata(&config.path);
        status.size_bytes = observed_size.as_ref().map_or(0, fs::Metadata::len);
        status.last_error = if self.thread.is_none() {
            Some("日志文件写入线程未能启动".into())
        } else {
            observed_size.err().and_then(|error| {
                (error.kind() != std::io::ErrorKind::NotFound)
                    .then(|| format!("无法读取日志文件状态：{error}"))
            })
        };
        Ok(())
    }

    pub(super) fn status(&self) -> LogPersistenceStatus {
        self.sender.status.read().clone()
    }
}

#[derive(Default)]
struct BoundedLogFile {
    path: Option<PathBuf>,
    size: u64,
}

impl BoundedLogFile {
    fn append(&mut self, config: &LogFileConfig, entry: &LogEntry) -> LogPersistenceResult<u64> {
        self.synchronize_path(config)?;
        let data = format_log_entries(std::slice::from_ref(entry)).into_bytes();
        let max_bytes = usize::try_from(config.max_bytes)
            .map_err(|_| std::io::Error::other("平台无法表示日志大小上限"))?;
        if data.len() >= max_bytes {
            let start = data.len().saturating_sub(max_bytes);
            let mut tail = data[start..].to_vec();
            trim_partial_utf8_prefix(&mut tail);
            fs::write(&config.path, &tail)?;
            self.size = u64::try_from(tail.len()).unwrap_or(u64::MAX);
            return Ok(self.size);
        }
        let data_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if self.size.saturating_add(data_bytes) <= config.max_bytes {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config.path)?
                .write_all(&data)?;
            self.size = self.size.saturating_add(data_bytes);
            return Ok(self.size);
        }
        self.compact_and_append(config, &data, max_bytes)
    }

    fn synchronize_path(&mut self, config: &LogFileConfig) -> LogPersistenceResult<()> {
        if self.path.as_ref() == Some(&config.path) {
            return Ok(());
        }
        if let Some(parent) = config
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        self.size = fs::metadata(&config.path).map_or(0, |metadata| metadata.len());
        self.path = Some(config.path.clone());
        Ok(())
    }

    fn compact_and_append(
        &mut self,
        config: &LogFileConfig,
        data: &[u8],
        max_bytes: usize,
    ) -> LogPersistenceResult<u64> {
        let retained_target = config.max_bytes.saturating_mul(COMPACTION_RETAIN_PERCENT) / 100;
        let retained_target = usize::try_from(retained_target).unwrap_or(max_bytes);
        let keep_bytes = retained_target.saturating_sub(data.len() + TRUNCATE_MARKER.len());
        let tail = read_utf8_tail(&config.path, keep_bytes)?;
        let mut rewritten = Vec::with_capacity(tail.len() + TRUNCATE_MARKER.len() + data.len());
        rewritten.extend_from_slice(&tail);
        rewritten.extend_from_slice(TRUNCATE_MARKER);
        rewritten.extend_from_slice(data);
        if rewritten.len() > max_bytes {
            let start = rewritten.len() - max_bytes;
            rewritten.drain(..start);
            trim_partial_utf8_prefix(&mut rewritten);
        }
        fs::write(&config.path, &rewritten)?;
        self.size = u64::try_from(rewritten.len()).unwrap_or(u64::MAX);
        Ok(self.size)
    }
}

fn read_utf8_tail(path: &Path, bytes: usize) -> LogPersistenceResult<Vec<u8>> {
    if bytes == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX).min(length);
    let offset = i64::try_from(bytes).map_err(|_| std::io::Error::other("日志尾部读取范围过大"))?;
    file.seek(SeekFrom::End(-offset))?;
    let mut tail = Vec::with_capacity(usize::try_from(bytes).unwrap_or_default());
    file.read_to_end(&mut tail)?;
    trim_partial_utf8_prefix(&mut tail);
    Ok(tail)
}

fn trim_partial_utf8_prefix(bytes: &mut Vec<u8>) {
    for prefix in 0..bytes.len().min(4) {
        if std::str::from_utf8(&bytes[prefix..]).is_ok() {
            bytes.drain(..prefix);
            return;
        }
    }
    if std::str::from_utf8(bytes).is_err() {
        bytes.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_path(name: &str) -> PathBuf {
        let sequence = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "zenclash-log-file-{name}-{}-{sequence}.log",
            std::process::id()
        ))
    }

    fn entry(payload: &str, timestamp_ms: u64) -> LogEntry {
        LogEntry {
            level: "info".into(),
            payload: payload.into(),
            timestamp_ms,
        }
    }

    #[test]
    fn compaction_keeps_the_latest_entry_below_the_limit() {
        let path = test_path("compact");
        let config = LogFileConfig::from_bytes(path.clone(), true, 180);
        let mut writer = BoundedLogFile::default();
        writer.append(&config, &entry(&"旧".repeat(50), 1)).unwrap();

        let size = writer
            .append(&config, &entry("latest.example.com", 2))
            .unwrap();
        let content = fs::read_to_string(&path).unwrap();

        assert!(size <= 180);
        assert!(content.contains("latest.example.com"));
        assert!(content.contains("Log compacted"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn disabled_configuration_does_not_create_a_file() {
        let path = test_path("disabled");
        let config = LogFileConfig::from_bytes(path.clone(), false, 180);
        let mut writer = BoundedLogFile::default();

        if config.enabled {
            writer.append(&config, &entry("ignored", 1)).unwrap();
        }

        assert!(!path.exists());
    }

    #[test]
    fn configuration_rejects_an_unbounded_limit() {
        let error = LogFileConfig::from_mebibytes(test_path("invalid"), true, 0).unwrap_err();

        assert!(matches!(error, LogPersistenceError::InvalidLimit));
    }

    #[test]
    fn worker_persists_a_configured_entry_and_reports_its_size() {
        let path = test_path("worker");
        let worker = LogFileWorker::start();
        worker.configure(path.clone(), true, 1).unwrap();

        worker.sender().append(entry("queued.example.com", 42));
        for _ in 0..100 {
            if worker.status().size_bytes > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("queued.example.com"));
        fs::remove_file(path).unwrap();
    }
}
