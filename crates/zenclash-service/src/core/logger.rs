use std::{collections::VecDeque, time::Duration};

use anyhow::Result;
use flexi_logger::{
    Cleanup, Criterion, DeferredNow, FileSpec, Naming, WriteMode,
    writers::{FileLogWriter, FileLogWriterBuilder, LogWriter as _},
};
use log::{Level, Record};
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};

use crate::core::structure::WriterConfig;

// Buffered writes avoid flexi's one-syscall-per-line default; the background
// flusher bounds the hard-kill loss window.
const WRITE_BUFFER_CAPACITY: usize = 64 * 1024;
const WRITE_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

static GLOBAL_WRITER: RwLock<Option<FileLogWriter>> = RwLock::new(None);

fn service_writer_builder(config: &WriterConfig) -> FileLogWriterBuilder {
    FileLogWriter::builder(
        FileSpec::default()
            .directory(config.directory.clone())
            .basename("service")
            .suppress_timestamp(),
    )
    .format(tracing_estuary::file_format_without_level)
    .write_mode(WriteMode::BufferAndFlushWith(
        WRITE_BUFFER_CAPACITY,
        WRITE_FLUSH_INTERVAL,
    ))
    .rotate(
        Criterion::Size(config.max_log_size),
        Naming::TimestampsCustomFormat {
            current_infix: Some("latest"),
            format: "%Y-%m-%d_%H-%M-%S",
        },
        Cleanup::KeepLogFiles(config.max_log_files),
    )
}

/// Resets the writer in place instead of rebuilding it: flexi's flusher thread
/// cannot be stopped, so every dropped writer would leak a thread and an fd.
pub fn set_or_update_writer(config: &WriterConfig) -> Result<()> {
    let builder = service_writer_builder(config);
    let mut slot = GLOBAL_WRITER.write();
    match slot.as_ref() {
        Some(writer) => {
            // Propagate the drain failure so the buffered tail stays retryable.
            writer.flush()?;
            writer.reset(&builder)?;
        }
        None => *slot = Some(builder.try_build()?),
    }
    Ok(())
}

pub fn write_core_line(level: Level, line: &str) {
    let guard = GLOBAL_WRITER.read();
    if let Some(writer) = guard.as_ref() {
        let mut now = DeferredNow::default();
        let args = format_args!("{line}");
        let record = Record::builder().args(args).level(level).target("service").build();
        let _ = writer.write(&mut now, &record);
    }
}

pub fn flush_writer() {
    let guard = GLOBAL_WRITER.read();
    if let Some(writer) = guard.as_ref() {
        let _ = writer.flush();
    }
}

const LOGS_QUEUE_LEN: usize = 100;

pub struct LogRing {
    inner: Mutex<VecDeque<String>>,
}

impl LogRing {
    fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(LOGS_QUEUE_LEN)),
        }
    }

    pub fn append_log(&self, log: String) {
        let mut guard = self.inner.lock();
        if guard.len() >= LOGS_QUEUE_LEN {
            guard.pop_front();
        }
        guard.push_back(log);
    }

    pub fn get_logs(&self) -> Vec<String> {
        let guard = self.inner.lock();
        guard.iter().cloned().collect()
    }

    pub fn clear_logs(&self) {
        self.inner.lock().clear();
    }
}

pub static LOG_RING: Lazy<LogRing> = Lazy::new(LogRing::new);
