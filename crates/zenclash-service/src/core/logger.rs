// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Lightweight rotating file writer and bounded output ring buffer backed by `std::fs` and `tracing`.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tracing::Level;

use crate::core::structure::WriterConfig;
use crate::protocol::{MAX_LOG_RING_BYTES, MAX_LOG_RING_LINES};

const WRITE_BUFFER_CAPACITY: usize = 64 * 1024;
const AUTO_FLUSH_BYTES: usize = 4 * 1024;
const AUTO_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const ACTIVE_LOG_FILE_NAME: &str = "service_latest.log";
const ROTATED_LOG_PREFIX: &str = "service_";
const ROTATED_LOG_SUFFIX: &str = ".log";

static ROTATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static GLOBAL_WRITER: Lazy<Mutex<Option<RotatingFileWriter>>> = Lazy::new(|| Mutex::new(None));

struct RotatingFileWriter {
    directory: PathBuf,
    max_log_size: u64,
    max_log_files: usize,
    file: Option<BufWriter<File>>,
    current_size: u64,
    unflushed_bytes: usize,
    last_flush: Instant,
}

impl RotatingFileWriter {
    fn open(config: &WriterConfig) -> Result<Self> {
        let directory = PathBuf::from(&config.directory);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create service log directory {directory:?}"))?;
        let active_path = directory.join(ACTIVE_LOG_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)
            .with_context(|| format!("failed to open service log file {active_path:?}"))?;
        let current_size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Ok(Self {
            directory,
            max_log_size: config.max_log_size,
            max_log_files: config.max_log_files,
            file: Some(BufWriter::with_capacity(WRITE_BUFFER_CAPACITY, file)),
            current_size,
            unflushed_bytes: 0,
            last_flush: Instant::now(),
        })
    }

    fn write_line(&mut self, level: Level, line: &str) -> Result<()> {
        let sanitized = line.trim_end_matches(['\r', '\n']);
        let written = sanitized.len().saturating_add(1);
        if self.max_log_size > 0
            && self.current_size > 0
            && self.current_size.saturating_add(written as u64) > self.max_log_size
        {
            self.rotate()?;
        }
        let writer = self.ensure_open()?;
        writer.write_all(sanitized.as_bytes())?;
        writer.write_all(b"\n")?;
        self.current_size = self.current_size.saturating_add(written as u64);
        self.unflushed_bytes = self.unflushed_bytes.saturating_add(written);
        if self.unflushed_bytes >= AUTO_FLUSH_BYTES
            || self.last_flush.elapsed() >= AUTO_FLUSH_INTERVAL
            || level == Level::ERROR
        {
            self.flush()?;
        }
        Ok(())
    }

    fn ensure_open(&mut self) -> Result<&mut BufWriter<File>> {
        if self.file.is_none() {
            let active_path = self.directory.join(ACTIVE_LOG_FILE_NAME);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&active_path)
                .with_context(|| format!("failed to reopen service log file {active_path:?}"))?;
            self.current_size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            self.file = Some(BufWriter::with_capacity(WRITE_BUFFER_CAPACITY, file));
        }
        self.file
            .as_mut()
            .context("service log file writer is unexpectedly unavailable")
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(writer) = self.file.as_mut() {
            writer.flush()?;
        }
        self.unflushed_bytes = 0;
        self.last_flush = Instant::now();
        Ok(())
    }

    fn rotate(&mut self) -> Result<()> {
        self.flush()?;
        // Release the open handle before renaming so rotation succeeds cleanly on Windows as well as Unix.
        self.file.take();

        let active_path = self.directory.join(ACTIVE_LOG_FILE_NAME);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let sequence = ROTATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let rotated_path = self.directory.join(format!(
            "{ROTATED_LOG_PREFIX}{timestamp}_{sequence:04}{ROTATED_LOG_SUFFIX}"
        ));

        let rotated = std::fs::rename(&active_path, &rotated_path).is_ok();
        let fresh = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&active_path)
            .with_context(|| format!("failed to reopen rotated service log file {active_path:?}"))?;
        self.file = Some(BufWriter::with_capacity(WRITE_BUFFER_CAPACITY, fresh));
        self.current_size = 0;
        self.unflushed_bytes = 0;
        self.last_flush = Instant::now();
        if rotated {
            prune_rotated_logs(&self.directory, self.max_log_files);
        }
        Ok(())
    }
}

fn prune_rotated_logs(directory: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut rotated: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            if name != ACTIVE_LOG_FILE_NAME
                && name.starts_with(ROTATED_LOG_PREFIX)
                && name.ends_with(ROTATED_LOG_SUFFIX)
            {
                Some((name, entry.path()))
            } else {
                None
            }
        })
        .collect();
    if rotated.len() <= keep {
        return;
    }
    rotated.sort_by(|left, right| left.0.cmp(&right.0));
    let remove_count = rotated.len().saturating_sub(keep);
    for (_, path) in rotated.into_iter().take(remove_count) {
        let _ = std::fs::remove_file(path);
    }
}

/// Configures or updates the active core log file writer without leaking background threads.
pub fn set_or_update_writer(config: &WriterConfig) -> Result<()> {
    let target_dir = PathBuf::from(&config.directory);
    let mut slot = GLOBAL_WRITER.lock();
    if let Some(writer) = slot.as_mut() {
        writer.flush()?;
        if writer.directory == target_dir {
            writer.max_log_size = config.max_log_size;
            writer.max_log_files = config.max_log_files;
            return Ok(());
        }
    }
    let writer = RotatingFileWriter::open(config)?;
    *slot = Some(writer);
    Ok(())
}

pub fn write_core_line(level: Level, line: &str) {
    let mut guard = GLOBAL_WRITER.lock();
    if let Some(writer) = guard.as_mut() {
        let _ = writer.write_line(level, line);
    }
}

pub fn flush_writer() {
    let mut guard = GLOBAL_WRITER.lock();
    if let Some(writer) = guard.as_mut() {
        let _ = writer.flush();
    }
}

struct LogRingState {
    lines: VecDeque<String>,
    total_bytes: usize,
    truncated: bool,
}

/// Bounded in-memory log ring buffer enforcing both line count and byte limits.
pub struct LogRing {
    inner: Mutex<LogRingState>,
    max_lines: usize,
    max_bytes: usize,
}

impl LogRing {
    fn new() -> Self {
        Self::with_limits(MAX_LOG_RING_LINES, MAX_LOG_RING_BYTES)
    }

    fn with_limits(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(LogRingState {
                lines: VecDeque::with_capacity(max_lines.min(MAX_LOG_RING_LINES)),
                total_bytes: 0,
                truncated: false,
            }),
            max_lines,
            max_bytes,
        }
    }

    pub fn append_log(&self, mut log: String) {
        if self.max_lines == 0 || self.max_bytes == 0 {
            return;
        }
        let mut guard = self.inner.lock();
        if log.len() > self.max_bytes {
            truncate_utf8_in_place(&mut log, self.max_bytes);
            guard.truncated = true;
        }
        while guard.lines.len() >= self.max_lines || guard.total_bytes.saturating_add(log.len()) > self.max_bytes {
            let Some(evicted) = guard.lines.pop_front() else {
                break;
            };
            guard.total_bytes = guard.total_bytes.saturating_sub(evicted.len());
            guard.truncated = true;
        }
        guard.total_bytes = guard.total_bytes.saturating_add(log.len());
        guard.lines.push_back(log);
    }

    pub fn get_logs(&self) -> Vec<String> {
        let guard = self.inner.lock();
        guard.lines.iter().cloned().collect()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn snapshot(&self) -> (Vec<String>, bool) {
        let guard = self.inner.lock();
        (guard.lines.iter().cloned().collect(), guard.truncated)
    }

    pub fn clear_logs(&self) {
        let mut guard = self.inner.lock();
        guard.lines.clear();
        guard.total_bytes = 0;
        guard.truncated = false;
    }
}

fn truncate_utf8_in_place(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

pub static LOG_RING: Lazy<LogRing> = Lazy::new(LogRing::new);

#[cfg(test)]
mod tests {
    use super::{ACTIVE_LOG_FILE_NAME, LogRing, RotatingFileWriter};
    use crate::core::structure::WriterConfig;
    use tracing::Level;

    #[test]
    fn log_ring_enforces_line_limit_and_tracks_truncation() {
        let ring = LogRing::with_limits(3, 1024);
        for index in 0..5 {
            ring.append_log(format!("line-{index}"));
        }
        let (lines, truncated) = ring.snapshot();
        assert!(truncated);
        assert_eq!(lines, vec!["line-2", "line-3", "line-4"]);

        ring.clear_logs();
        let (lines, truncated) = ring.snapshot();
        assert!(!truncated);
        assert!(lines.is_empty());
    }

    #[test]
    fn log_ring_enforces_byte_limit_and_truncates_oversized_utf8_safely() {
        let ring = LogRing::with_limits(10, 12);
        ring.append_log("aaaaaa".to_owned());
        ring.append_log("bbbbbb".to_owned());
        assert_eq!(ring.get_logs(), vec!["aaaaaa", "bbbbbb"]);

        // Appending another 6-byte line evicts "aaaaaa" to stay within 12 bytes.
        ring.append_log("cccccc".to_owned());
        let (lines, truncated) = ring.snapshot();
        assert!(truncated);
        assert_eq!(lines, vec!["bbbbbb", "cccccc"]);

        // Multi-byte UTF-8 (3 bytes per char) exceeding 12 bytes truncates cleanly on char boundary.
        ring.append_log("中文日志超长输出".to_owned());
        let lines = ring.get_logs();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].len() <= 12);
        assert_eq!(lines[0], "中文日志");
    }

    #[test]
    fn rotating_file_writer_rotates_and_prunes_old_files() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!(
            "zenclash-logger-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let config = WriterConfig {
            directory: root.to_string_lossy().into_owned(),
            max_log_size: 32,
            max_log_files: 2,
        };
        let mut writer = RotatingFileWriter::open(&config)?;
        for index in 0..8 {
            writer.write_line(Level::INFO, &format!("core-output-line-{index:02}-padding"))?;
        }
        writer.flush()?;

        let active = std::fs::read_to_string(root.join(ACTIVE_LOG_FILE_NAME))?;
        assert!(active.contains("core-output-line-07-padding"));

        let rotated_count = std::fs::read_dir(&root)?
            .flatten()
            .filter(|entry| entry.file_name() != ACTIVE_LOG_FILE_NAME)
            .count();
        assert!(
            rotated_count <= 2,
            "expected at most 2 rotated log files, found {rotated_count}"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
