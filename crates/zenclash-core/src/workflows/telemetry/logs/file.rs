//! Serialized, bounded persistence for the live Mihomo log stream.

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use thiserror::Error;

use super::{LogEntry, format_log_entries, private_io};

const MEBIBYTE: u64 = 1024 * 1024;
const MIN_MAX_MEBIBYTES: u16 = 1;
const MAX_MAX_MEBIBYTES: u16 = 100;
const FILE_QUEUE_CAPACITY: usize = 512;
const FILE_QUEUE_BYTES: usize = 4 * 1024 * 1024;
const MAX_WRITE_BATCH_ENTRIES: usize = 64;
const MAX_WRITE_BATCH_BYTES: usize = 64 * 1024;
const COMPACTION_RETAIN_PERCENT: u64 = 50;
const TRUNCATE_MARKER: &[u8] =
    b"\n[ZENCLASH] Log compacted after reaching the configured size limit.\n";

/// Errors produced while configuring or writing continuous Mihomo logs.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LogPersistenceError {
    /// The configured limit is outside the supported range.
    #[error("{}", zenclash_i18n::text("logs.errors.invalid_limit"))]
    InvalidLimit,
    /// Filesystem access failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("logs.errors.io", &[("error", .0.to_string())])
    )]
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
    /// Entries discarded because of queue capacity or the per-entry byte limit.
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
    Append(Arc<LogEntry>, LogByteReservation),
    Refresh,
    Shutdown,
}

struct LogByteReservation {
    queued_bytes: Arc<AtomicUsize>,
    queued_entries: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for LogByteReservation {
    fn drop(&mut self) {
        self.queued_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.queued_entries.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub(super) struct LogFileSender {
    sender: Arc<RwLock<Option<SyncSender<LogFileCommand>>>>,
    settings: Arc<RwLock<Option<LogFileConfig>>>,
    status: Arc<RwLock<LogPersistenceStatus>>,
    queued_bytes: Arc<AtomicUsize>,
    queued_entries: Arc<AtomicUsize>,
}

impl LogFileSender {
    pub(super) fn append(&self, entry: Arc<LogEntry>, bytes: usize) {
        if !self
            .settings
            .read()
            .as_ref()
            .is_some_and(|settings| settings.enabled)
        {
            return;
        }
        let Some(channel) = self.sender.read().clone() else {
            return;
        };
        if bytes > super::MAX_LOG_ENTRY_BYTES {
            let mut status = self.status.write();
            status.dropped_entries = status.dropped_entries.saturating_add(1);
            status.last_error = Some(zenclash_i18n::text("logs.errors.entry_too_large"));
            return;
        }
        let reserved = self
            .queued_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= FILE_QUEUE_BYTES)
            })
            .is_ok();
        if !reserved {
            self.record_full_queue();
            return;
        }
        self.queued_entries.fetch_add(1, Ordering::AcqRel);
        let reservation = LogByteReservation {
            queued_bytes: self.queued_bytes.clone(),
            queued_entries: self.queued_entries.clone(),
            bytes,
        };
        match channel.try_send(LogFileCommand::Append(entry, reservation)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.record_full_queue();
            }
            Err(TrySendError::Disconnected(_)) => {
                self.status.write().last_error = Some("日志文件写入线程已停止".into());
            }
        }
    }

    fn record_full_queue(&self) {
        let mut status = self.status.write();
        status.dropped_entries = status.dropped_entries.saturating_add(1);
        status.last_error = Some("日志磁盘队列已满；已丢弃一条落盘记录".into());
    }
}

pub(super) struct LogFileWorker {
    sender: LogFileSender,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Drop for LogFileWorker {
    fn drop(&mut self) {
        if let Some(channel) = self.sender.sender.write().take() {
            let _ = channel.try_send(LogFileCommand::Shutdown);
            drop(channel);
        }
        if let Some(handle) = self.thread.get_mut().take() {
            let _ = handle.join();
        }
    }
}

impl LogFileWorker {
    /// Creates a lazy log-file worker without spawning an OS thread until
    /// persistence is explicitly enabled via [`Self::configure`].
    pub(super) fn start() -> Self {
        let file_sender = LogFileSender {
            sender: Arc::new(RwLock::new(None)),
            settings: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(LogPersistenceStatus::default())),
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            queued_entries: Arc::default(),
        };
        Self {
            sender: file_sender,
            thread: Mutex::new(None),
        }
    }

    fn ensure_thread_started(&self) {
        let mut handle = self.thread.lock();
        if handle.is_some() {
            return;
        }
        let (tx, receiver) = mpsc::sync_channel(FILE_QUEUE_CAPACITY);
        *self.sender.sender.write() = Some(tx);
        let thread_settings = self.sender.settings.clone();
        let thread_status = self.sender.status.clone();
        match thread::Builder::new()
            .name("zenclash-log-file".into())
            .spawn(move || run_log_file_worker(receiver, thread_settings, thread_status))
        {
            Ok(thread) => {
                *handle = Some(thread);
            }
            Err(error) => {
                *self.sender.sender.write() = None;
                self.sender.status.write().last_error =
                    Some(format!("无法启动日志文件线程：{error}"));
            }
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
        if status.path.as_ref() != Some(&path) {
            status.size_bytes = 0;
            status.last_error = None;
        }
        status.path = Some(path);
        status.max_bytes = config.max_bytes;
        drop(status);
        if enabled {
            self.ensure_thread_started();
            if self.thread.lock().is_none() {
                self.sender.status.write().last_error = Some("日志文件写入线程未能启动".into());
            }
        }
        // A full queue already wakes the worker; periodic refresh is the fallback.
        if let Some(channel) = self.sender.sender.read().as_ref() {
            let _ = channel.try_send(LogFileCommand::Refresh);
        } else {
            refresh_file_status(&self.sender.status, &config);
        }
        Ok(())
    }

    pub(super) fn pending_entries(&self) -> usize {
        self.sender.queued_entries.load(Ordering::Acquire)
    }

    pub(super) fn status(&self) -> LogPersistenceStatus {
        self.sender.status.read().clone()
    }
}

fn run_log_file_worker(
    receiver: mpsc::Receiver<LogFileCommand>,
    thread_settings: Arc<RwLock<Option<LogFileConfig>>>,
    thread_status: Arc<RwLock<LogPersistenceStatus>>,
) {
    let mut writer = BoundedLogFile::default();
    let mut observed = Instant::now();
    loop {
        let command = match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(LogFileCommand::Shutdown) => break,
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let config = thread_settings.read().clone();
        let Some(config) = config else {
            continue;
        };
        let mut refresh_requested = matches!(command, Some(LogFileCommand::Refresh));
        let mut shutdown_requested = false;
        if let Some(LogFileCommand::Append(first_entry, first_reservation)) = command {
            let mut batch_entries = Vec::with_capacity(MAX_WRITE_BATCH_ENTRIES);
            let mut batch_reservations = Vec::with_capacity(MAX_WRITE_BATCH_ENTRIES);
            let mut batch_bytes = first_reservation.bytes;
            batch_entries.push((*first_entry).clone());
            batch_reservations.push(first_reservation);
            while batch_entries.len() < MAX_WRITE_BATCH_ENTRIES
                && batch_bytes < MAX_WRITE_BATCH_BYTES
            {
                match receiver.try_recv() {
                    Ok(LogFileCommand::Append(entry, reservation)) => {
                        batch_bytes = batch_bytes.saturating_add(reservation.bytes);
                        batch_entries.push((*entry).clone());
                        batch_reservations.push(reservation);
                    }
                    Ok(LogFileCommand::Refresh) => {
                        refresh_requested = true;
                    }
                    Ok(LogFileCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                        shutdown_requested = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if config.enabled {
                let result = writer.append_batch(&config, &batch_entries);
                drop(batch_entries);
                drop(batch_reservations);
                let mut snapshot = thread_status.write();
                if snapshot.path.as_ref() == Some(&config.path) {
                    match result {
                        Ok(size) => {
                            snapshot.size_bytes = size;
                            snapshot.last_error = None;
                        }
                        Err(error) => snapshot.last_error = Some(error.to_string()),
                    }
                }
            } else {
                drop(batch_entries);
                drop(batch_reservations);
            }
        }
        if observed.elapsed() >= Duration::from_secs(1) || refresh_requested {
            refresh_file_status(&thread_status, &config);
            observed = Instant::now();
        }
        if shutdown_requested {
            break;
        }
    }
}

fn refresh_file_status(status: &RwLock<LogPersistenceStatus>, config: &LogFileConfig) {
    let metadata = fs::metadata(&config.path);
    let mut status = status.write();
    if status.path.as_ref() != Some(&config.path) {
        return;
    }
    match metadata {
        Ok(metadata) => status.size_bytes = metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => status.size_bytes = 0,
        Err(error) if status.last_error.is_none() => {
            status.last_error = Some(format!("无法读取日志文件状态：{error}"));
        }
        Err(_) => {}
    }
}

#[derive(Default)]
struct BoundedLogFile {
    path: Option<PathBuf>,
    size: u64,
    write_access: Option<crate::platform::data_coordinator::DataWriteAccess>,
}

impl BoundedLogFile {
    #[cfg(test)]
    fn append(&mut self, config: &LogFileConfig, entry: &LogEntry) -> LogPersistenceResult<u64> {
        self.append_batch(config, std::slice::from_ref(entry))
    }

    fn append_batch(
        &mut self,
        config: &LogFileConfig,
        entries: &[LogEntry],
    ) -> LogPersistenceResult<u64> {
        if entries.is_empty() {
            return Ok(self.size);
        }
        if self.path.as_ref() != Some(&config.path) {
            self.write_access = None;
        }
        let _write_lease = self
            .write_access
            .get_or_insert_with(|| {
                crate::platform::data_coordinator::DataWriteAccess::new(&config.path)
            })
            .acquire()?;
        self.synchronize_path(config)?;
        let data = format_log_entries(entries).into_bytes();
        let max_bytes = usize::try_from(config.max_bytes)
            .map_err(|_| std::io::Error::other("平台无法表示日志大小上限"))?;
        if data.len() >= max_bytes {
            let start = data.len().saturating_sub(max_bytes);
            let mut tail = data[start..].to_vec();
            sanitize_utf8_tail(&mut tail);
            drain_to_max_utf8_bytes(&mut tail, max_bytes);
            private_io::replace(&config.path, &tail)?;
            self.size = u64::try_from(tail.len()).unwrap_or(u64::MAX);
            return Ok(self.size);
        }
        let data_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if self.size.saturating_add(data_bytes) <= config.max_bytes {
            private_io::append(&config.path, &data)?;
            self.size = self.size.saturating_add(data_bytes);
            return Ok(self.size);
        }
        self.compact_and_append(config, &data, max_bytes)
    }

    fn synchronize_path(&mut self, config: &LogFileConfig) -> LogPersistenceResult<()> {
        if self.path.as_ref() != Some(&config.path) {
            if let Some(parent) = config
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            self.path = Some(config.path.clone());
        }
        // Configuration events, another app instance, or manual maintenance
        // may change the file without changing its path. Refresh before every
        // append so the bounded writer and the UI never return to a stale
        // cached counter after reporting the real metadata size.
        self.size = fs::metadata(&config.path).map_or(0, |metadata| metadata.len());
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
            sanitize_utf8_tail(&mut rewritten);
            drain_to_max_utf8_bytes(&mut rewritten, max_bytes);
        }
        private_io::replace(&config.path, &rewritten)?;
        self.size = u64::try_from(rewritten.len()).unwrap_or(u64::MAX);
        Ok(self.size)
    }
}

fn read_utf8_tail(path: &Path, max_bytes: usize) -> LogPersistenceResult<Vec<u8>> {
    if max_bytes == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(Vec::new());
    }
    let keep = u64::try_from(max_bytes).unwrap_or(u64::MAX).min(length);
    let sliced_mid_file = keep < length;
    let read_len = if sliced_mid_file {
        keep.saturating_add(1)
    } else {
        keep
    };
    let offset =
        i64::try_from(read_len).map_err(|_| std::io::Error::other("日志尾部读取范围过大"))?;
    file.seek(SeekFrom::End(-offset))?;
    let mut tail = Vec::with_capacity(usize::try_from(read_len).unwrap_or_default());
    file.read_to_end(&mut tail)?;
    if sliced_mid_file {
        // Check whether the byte immediately preceding the retained slice was a newline
        // so already-aligned lines are kept intact while mid-line slices advance to the
        // next complete line boundary.
        let started_at_line_boundary = tail.first().copied() == Some(b'\n');
        if !tail.is_empty() {
            tail.drain(..1);
        }
        if !started_at_line_boundary
            && let Some(newline) = tail.iter().position(|byte| *byte == b'\n')
            && newline + 1 < tail.len()
        {
            tail.drain(..=newline);
        }
    }
    sanitize_utf8_tail(&mut tail);
    drain_to_max_utf8_bytes(&mut tail, max_bytes);
    Ok(tail)
}

fn sanitize_utf8_tail(bytes: &mut Vec<u8>) {
    let continuation_prefix = bytes
        .iter()
        .take(3)
        .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
        .count();
    if continuation_prefix > 0 {
        bytes.drain(..continuation_prefix);
    }
    if std::str::from_utf8(bytes).is_err() {
        // Replace only malformed byte sequences instead of clearing the entire log history.
        let sanitized = String::from_utf8_lossy(bytes).into_owned().into_bytes();
        *bytes = sanitized;
    }
}

fn drain_to_max_utf8_bytes(bytes: &mut Vec<u8>, max_bytes: usize) {
    if bytes.len() <= max_bytes {
        return;
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    bytes.drain(..start);
}

#[cfg(test)]
mod tests;
