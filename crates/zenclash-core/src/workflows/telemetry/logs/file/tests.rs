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
        ..LogEntry::default()
    }
}

fn queue_fixture(capacity: usize) -> (LogFileSender, mpsc::Receiver<LogFileCommand>) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (
        LogFileSender {
            sender: Arc::new(RwLock::new(Some(sender))),
            settings: Arc::new(RwLock::new(Some(LogFileConfig::from_bytes(
                test_path("queue"),
                true,
                MEBIBYTE,
            )))),
            status: Arc::default(),
            queued_bytes: Arc::default(),
            queued_entries: Arc::default(),
        },
        receiver,
    )
}

fn append_entry(sender: &LogFileSender, entry: Arc<LogEntry>) {
    let bytes = super::super::log_entry_bytes(&entry);
    sender.append(entry, bytes);
}

#[cfg(unix)]
#[test]
fn raw_log_permission_fixture_child() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("ZENCLASH_LOG_PERMISSION_FIXTURE_CHILD").is_none() {
        return;
    }
    // SAFETY: This dedicated test process has no other log/permission tests.
    unsafe {
        libc::umask(0o022);
    }
    let path = test_path("private-permissions");
    let config = LogFileConfig::from_bytes(path.clone(), true, 180);
    let mut writer = BoundedLogFile::default();
    writer.append(&config, &entry("first", 1)).unwrap();
    let created_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    writer.append(&config, &entry("second", 2)).unwrap();
    let appended_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    writer
        .append(&config, &entry(&"old".repeat(30), 3))
        .unwrap();
    writer.append(&config, &entry("latest", 4)).unwrap();
    let compacted_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    writer
        .append(&config, &entry(&"long".repeat(100), 5))
        .unwrap();
    let replaced_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    fs::remove_file(path).unwrap();
    assert_eq!(
        (created_mode, appended_mode, compacted_mode, replaced_mode),
        (0o600, 0o600, 0o600, 0o600)
    );
}

#[cfg(unix)]
#[test]
fn raw_log_creation_append_and_rewrite_are_private_under_permissive_umask() {
    let fixture = format!(
        "{}::raw_log_permission_fixture_child",
        module_path!().trim_start_matches("zenclash_core::")
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &fixture, "--nocapture"])
        .env("ZENCLASH_LOG_PERMISSION_FIXTURE_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "child selected no test: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pending_entries_include_inflight_and_release_on_full_and_disconnect() {
    let (sender, receiver) = queue_fixture(1);
    let message = Arc::new(entry("queued", 1));
    append_entry(&sender, message.clone());
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 1);
    append_entry(&sender, message.clone());
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 1);
    let inflight = receiver.try_recv().unwrap();
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 1);
    drop(inflight);
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 0);
    append_entry(&sender, message.clone());
    drop(receiver);
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 0);
    append_entry(&sender, message);
    assert_eq!(sender.queued_entries.load(Ordering::Acquire), 0);
}

#[test]
fn byte_budget_rejects_before_entry_capacity_and_is_reusable_after_consumption() {
    let (sender, receiver) = queue_fixture(FILE_QUEUE_CAPACITY);
    let entry = Arc::new(entry(&"x".repeat(60 * 1024), 1));
    let bytes = super::super::log_entry_bytes(&entry);
    let admitted = FILE_QUEUE_BYTES / bytes;
    assert!(admitted < FILE_QUEUE_CAPACITY);
    for _ in 0..=admitted {
        sender.append(entry.clone(), bytes);
    }
    assert_eq!(
        sender.queued_bytes.load(Ordering::Acquire),
        admitted * bytes
    );
    assert_eq!(sender.status.read().dropped_entries, 1);

    let command = receiver.try_recv().unwrap();
    assert_eq!(
        sender.queued_bytes.load(Ordering::Acquire),
        admitted * bytes
    );
    let LogFileCommand::Append(queued_entry, _) = &command else {
        panic!("expected a queued log entry");
    };
    assert!(Arc::ptr_eq(queued_entry, &entry));
    drop(command);
    sender.append(entry, bytes);

    let pending = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(pending.len(), admitted);
    assert_eq!(sender.status.read().dropped_entries, 1);
    drop(pending);
    assert_eq!(sender.queued_bytes.load(Ordering::Acquire), 0);
}

#[test]
fn dropping_the_receiver_releases_all_queued_bytes_with_a_live_sender() {
    let (sender, receiver) = queue_fixture(FILE_QUEUE_CAPACITY);
    append_entry(&sender, Arc::new(entry("queued", 1)));
    assert!(sender.queued_bytes.load(Ordering::Acquire) > 0);

    drop(receiver);

    assert_eq!(sender.queued_bytes.load(Ordering::Acquire), 0);
    append_entry(&sender, Arc::new(entry("disconnected", 2)));
    assert_eq!(sender.queued_bytes.load(Ordering::Acquire), 0);
    assert!(sender.status.read().last_error.is_some());
}

#[test]
fn a_full_entry_queue_rolls_back_the_rejected_byte_reservation() {
    let (sender, receiver) = queue_fixture(1);
    let entry = Arc::new(entry("small", 1));
    let bytes = super::super::log_entry_bytes(&entry);
    sender.append(entry.clone(), bytes);
    sender.append(entry, bytes);

    assert_eq!(sender.queued_bytes.load(Ordering::Acquire), bytes);
    assert_eq!(sender.status.read().dropped_entries, 1);
    drop(receiver);
    assert_eq!(sender.queued_bytes.load(Ordering::Acquire), 0);
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
fn compaction_preserves_log_history_across_invalid_utf8_bytes() {
    let path = test_path("invalid-utf8-compact");
    let config = LogFileConfig::from_bytes(path.clone(), true, 600);
    let mut raw = Vec::new();
    for index in 0..10 {
        raw.extend_from_slice(format!("old-line-{index:02}: padding-content-aaaa\n").as_bytes());
    }
    raw.extend_from_slice(b"corrupted-line: ");
    raw.push(0xFF);
    raw.extend_from_slice(b" survived\n");
    raw.extend_from_slice(b"recent-valid-line-one: keep-me\n");
    raw.extend_from_slice(b"recent-valid-line-two: keep-me-too\n");
    assert!(raw.len() as u64 <= 600);
    // Push total size right up near the limit so the next append triggers compaction.
    while (raw.len() as u64) < 580 {
        raw.insert(0, b'x');
    }
    fs::write(&path, &raw).unwrap();

    let mut writer = BoundedLogFile::default();
    let size = writer
        .append(&config, &entry("brand-new.example.com", 99))
        .unwrap();
    let content = fs::read_to_string(&path).unwrap();

    assert!(size <= 600);
    assert!(content.contains("corrupted-line: \u{FFFD} survived"));
    assert!(content.contains("recent-valid-line-one: keep-me"));
    assert!(content.contains("recent-valid-line-two: keep-me-too"));
    assert!(content.contains("brand-new.example.com"));
    assert!(content.contains("Log compacted"));
    fs::remove_file(path).unwrap();
}

#[test]
fn compaction_aligns_retained_tail_to_newline_boundary() {
    let path = test_path("newline-aligned-compact");
    let config = LogFileConfig::from_bytes(path.clone(), true, 400);
    let mut content = String::new();
    for index in 0..12 {
        content.push_str(&format!("LINE-{index:02}: abcdefghijklmnopqrstuv\n"));
    }
    fs::write(&path, content.as_bytes()).unwrap();

    let mut writer = BoundedLogFile::default();
    let size = writer
        .append(&config, &entry("after-compaction", 10))
        .unwrap();
    let compacted = fs::read_to_string(&path).unwrap();
    let first_line = compacted.lines().next().unwrap();

    assert!(size <= 400);
    assert!(
        first_line.starts_with("LINE-"),
        "compacted tail must start on a complete line boundary, got: {first_line:?}"
    );
    assert!(compacted.contains("after-compaction"));
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

    append_entry(&worker.sender(), Arc::new(entry("queued.example.com", 42)));
    for _ in 0..100 {
        if worker.status().size_bytes > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("queued.example.com")
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn append_resynchronizes_after_the_file_changes_outside_the_writer() {
    let path = test_path("external-change");
    let config = LogFileConfig::from_bytes(path.clone(), true, 1_024);
    let mut writer = BoundedLogFile::default();
    writer.append(&config, &entry("first", 1)).unwrap();
    fs::write(&path, b"x").unwrap();

    let reported = writer.append(&config, &entry("second", 2)).unwrap();

    assert_eq!(reported, fs::metadata(&path).unwrap().len());
    assert!(fs::read_to_string(&path).unwrap().contains("second"));
    fs::remove_file(path).unwrap();
}

#[test]
fn worker_observes_external_file_changes_without_io_in_status_reads() {
    let path = test_path("status-metadata");
    let worker = LogFileWorker::start();
    worker.configure(path.clone(), true, 1).unwrap();
    fs::write(&path, b"external").unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while worker.status().size_bytes != 8 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(worker.status().size_bytes, 8);
    fs::remove_file(path).unwrap();
}

#[test]
fn worker_stays_lazy_until_enabled_and_joins_cleanly_on_drop() {
    let path = test_path("lazy-worker");
    let worker = LogFileWorker::start();
    assert!(worker.thread.lock().is_none());

    // Disabled configuration must NOT spawn a background thread.
    worker.configure(path.clone(), false, 1).unwrap();
    assert!(worker.thread.lock().is_none());

    // Enabling lazily spawns the writer thread, batches writes, and Drop joins cleanly.
    worker.configure(path.clone(), true, 1).unwrap();
    assert!(worker.thread.lock().is_some());
    let sender = worker.sender();
    for index in 0..10 {
        append_entry(&sender, Arc::new(entry(&format!("batch-{index}"), index)));
    }
    drop(sender);
    drop(worker);

    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("batch-0"));
    assert!(content.contains("batch-9"));
    fs::remove_file(path).unwrap();
}
