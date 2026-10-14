use super::*;
use crate::logs::MihomoLogLevel;
use crate::stream_publication_test_support::{PublicationPhase, publication_pause};
use std::sync::Arc;
use tokio::{runtime::Handle, sync::watch};

fn monitor_without_socket(runtime: &Handle) -> Arc<LogMonitor> {
    let (level, _) = watch::channel(MihomoLogLevel::Info);
    let (generation, _) = watch::channel(1);
    Arc::new(LogMonitor {
        entries: Arc::new(RwLock::new(LogBuffer::default())),
        stream: Arc::new(RwLock::new(LogStreamSnapshot {
            generation: 1,
            connected: true,
            updated_at_ms: 42,
            ..Default::default()
        })),
        revision: Arc::new(AtomicU64::new(0)),
        level,
        expected_generation: Arc::new(AtomicU64::new(1)),
        generation,
        file: file::LogFileWorker::start(),
        task: runtime.spawn(std::future::pending()),
    })
}

fn old_entry() -> LogEntry {
    LogEntry {
        level: "info".into(),
        payload: "old generation payload".into(),
        received_at_ms: 99,
        ..Default::default()
    }
}

#[test]
fn a_paused_old_log_entry_cannot_enter_history_or_persistence_after_transition() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let directory = std::env::temp_dir().join(format!(
        "zenclash-log-publication-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("runtime.log");
    monitor.configure_persistence(&path, true, 1).unwrap();
    let (pause, controller) = publication_pause(PublicationPhase::BeforeStateLock);
    let old = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        accept_log_entry(
            &old.entries,
            &old.stream,
            &old.revision,
            &old.file.sender(),
            old_entry(),
            1,
            &old.expected_generation,
        )
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    let snapshot = monitor.stream_snapshot();
    let revision = monitor.revision();
    controller.resume();
    let accepted = writer.join().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while monitor.pending_persistence_entries() != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let persisted = std::fs::read_to_string(&path).unwrap_or_default();
    let entries = monitor.entries();
    let completed_snapshot = monitor.stream_snapshot();
    let completed_revision = monitor.revision();
    drop(monitor);
    std::fs::remove_dir_all(directory).unwrap();
    assert!(!accepted);
    assert!(entries.is_empty());
    assert!(!persisted.contains("old generation payload"));
    assert_eq!(completed_snapshot, snapshot);
    assert_eq!(completed_revision, revision);
    assert_eq!(snapshot.updated_at_ms, 42);
    assert_eq!(revision, 1);
}

#[test]
fn a_paused_old_log_error_cannot_replace_current_health_or_append_history() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeStateLock);
    let old = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        push_monitor_error_for_generation(
            &old.entries,
            &old.stream,
            &old.revision,
            &old.file.sender(),
            "old socket failed".into(),
            1,
            &old.expected_generation,
        )
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    let snapshot = monitor.stream_snapshot();
    let revision = monitor.revision();
    controller.resume();
    assert!(!writer.join().unwrap());
    assert_eq!(monitor.stream_snapshot(), snapshot);
    assert!(monitor.entries().is_empty());
    assert_eq!(monitor.revision(), revision);
}

#[test]
fn a_paused_old_log_connection_cannot_reconnect_a_replaced_generation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeStateLock);
    let old = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        update_log_connection(&old.stream, true, None, 1, &old.expected_generation)
    });
    controller.wait_until_reached();
    monitor.synchronize_generation(2);
    let snapshot = monitor.stream_snapshot();
    controller.resume();
    assert!(!writer.join().unwrap());
    assert_eq!(monitor.stream_snapshot(), snapshot);
}

#[test]
fn log_keeps_the_generation_gate_until_buffer_and_persistence_admission_complete() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let monitor = monitor_without_socket(runtime.handle());
    let (pause, controller) = publication_pause(PublicationPhase::BeforeBufferAppend);
    let current = monitor.clone();
    let writer = std::thread::spawn(move || {
        pause.install();
        accept_log_entry(
            &current.entries,
            &current.stream,
            &current.revision,
            &current.file.sender(),
            old_entry(),
            1,
            &current.expected_generation,
        )
    });
    controller.wait_until_reached();
    let state_gate_held = monitor.stream.try_write().is_none();
    let revision_before_append = monitor.revision();
    controller.resume();
    assert!(writer.join().unwrap());
    assert!(
        state_gate_held,
        "generation transition must wait for buffer and persistence admission"
    );
    assert_eq!(revision_before_append, 0);
    assert_eq!(monitor.entries().len(), 1);
}
