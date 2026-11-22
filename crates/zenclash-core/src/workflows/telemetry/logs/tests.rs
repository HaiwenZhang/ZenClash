use super::*;

fn push_bounded(entries: &RwLock<LogBuffer>, entry: LogEntry) -> Option<BufferedLogEntry> {
    BufferedLogEntry::prepare(entry).map(|entry| push_prepared_bounded(entries, entry))
}

#[tokio::test]
async fn a_late_log_connection_error_never_falls_back_to_a_new_controller() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let old_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let new_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let old_endpoint =
        MihomoEndpoint::new(format!("http://{}", old_listener.local_addr().unwrap()), "");
    let new_endpoint =
        MihomoEndpoint::new(format!("http://{}", new_listener.local_addr().unwrap()), "");
    let client = MihomoClient::new(old_endpoint).unwrap();
    let binding = client.binding.clone();
    let (ready, started) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let old_server = tokio::spawn(async move {
        let (mut socket, _) =
            tokio::time::timeout(std::time::Duration::from_secs(3), old_listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut bytes = [0_u8; 1024];
            let count =
                tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&bytes[..count]);
            assert!(request.len() <= 8192);
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /logs?level=warning&format=structured HTTP/1.1")
        );
        ready.send(()).unwrap();
        resume.await.unwrap();
        // Closing without a handshake releases the original failed attempt.
        drop(socket);
    });
    let caller_binding = binding.clone();
    let opening =
        tokio::spawn(
            async move { connect_log_stream(&caller_binding, MihomoLogLevel::Warning).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(3), started)
        .await
        .unwrap()
        .unwrap();
    // Publish through the actual binding; no synthetic generation flag.
    client.switch_to_direct(new_endpoint).await.unwrap();
    let new_server = tokio::spawn(async move {
        let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), new_listener.accept())
                .await
        else {
            return false;
        };
        socket
            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        true
    });
    release.send(()).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), opening)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    old_server.await.unwrap();
    assert!(
        !new_server.await.unwrap(),
        "plain fallback connected to the new controller"
    );
}

#[test]
fn parses_real_log_frame() {
    let entry = parse_log_frame(
        br#"{"type":"info","payload":"[TCP] connected to example.com:443"}"#,
        42,
    )
    .unwrap();
    assert_eq!(entry.level, "info");
    assert!(entry.payload.contains("example.com"));
    assert_eq!(entry.timestamp_ms, 42);
    assert_eq!(entry.received_at_ms, 42);
    assert_eq!(entry.time_source, LogTimeSource::LocalReceive);
}

#[test]
fn structured_log_preserves_core_time_level_message_and_fields() {
    let entry = parse_log_frame(
        br#"{"time":"2026-08-27T12:34:56.789Z","level":"warning","message":"dial failed","fields":{"network":"tcp","attempt":2}}"#,
        1,
    )
    .unwrap();

    assert_eq!(entry.level, "warning");
    assert_eq!(entry.payload, "dial failed");
    assert_eq!(entry.core_time.as_deref(), Some("2026-08-27T12:34:56.789Z"));
    assert_eq!(entry.fields["network"], "tcp");
    assert_eq!(entry.fields["attempt"], 2);
    assert_eq!(entry.timestamp_ms, 1_787_834_096_789);
    assert_eq!(entry.time_source, LogTimeSource::Core);
    assert_eq!(
        entry.filtered_fields(&["network"]),
        BTreeMap::from([("network".into(), Value::String("tcp".into()))])
    );
}

#[test]
fn structured_log_preserves_array_shaped_mihomo_fields() {
    let entry = parse_log_frame(
        br#"{"time":"2026-08-27T12:34:56Z","level":"info","message":"ready","fields":["listener","mixed"]}"#,
        1,
    )
    .unwrap();

    assert_eq!(entry.fields, serde_json::json!(["listener", "mixed"]));
    assert_eq!(entry.time_source, LogTimeSource::Core);
    assert!(entry.filtered_fields(&["listener"]).is_empty());
}

#[test]
fn structured_time_only_uses_the_closest_local_date() {
    let received = Local
        .with_ymd_and_hms(2026, 8, 28, 0, 0, 1)
        .single()
        .expect("unambiguous local test time");
    let entry = parse_log_frame(
        br#"{"time":"23:59:59","level":"info","message":"ready","fields":[]}"#,
        u64::try_from(received.timestamp_millis()).unwrap(),
    )
    .unwrap();
    let expected = Local
        .with_ymd_and_hms(2026, 8, 27, 23, 59, 59)
        .single()
        .expect("unambiguous local test time");

    assert_eq!(
        entry.timestamp_ms,
        u64::try_from(expected.timestamp_millis()).unwrap()
    );
    assert_eq!(entry.time_source, LogTimeSource::Core);
}

#[test]
fn text_export_preserves_order_and_severity() {
    let entries = [
        LogEntry {
            level: "info".into(),
            payload: "first".into(),
            timestamp_ms: 10,
            ..LogEntry::default()
        },
        LogEntry {
            level: "warn".into(),
            payload: "second".into(),
            timestamp_ms: 20,
            ..LogEntry::default()
        },
    ];

    assert_eq!(
        format_log_entries(&entries),
        "[10] [local-receive] INFO    first\n[20] [local-receive] WARN    second\n"
    );
}

#[test]
fn support_safe_log_copy_omits_messages_fields_and_core_time() {
    let entry = LogEntry {
        level: "info".into(),
        payload: "Bearer secret https://example.com/?token=raw /Users/alice/config".into(),
        core_time: Some("private-core-time".into()),
        fields: serde_json::json!({"password": "secret"}),
        timestamp_ms: 10,
        received_at_ms: 10,
        time_source: LogTimeSource::Core,
    };

    let safe = format_log_entries_support_safe(&[entry]);

    assert_eq!(safe, "[10] [core] INFO [message omitted]\n");
}

#[test]
fn repeated_connection_error_updates_timestamp_without_flooding_log() {
    let entries = RwLock::new(LogBuffer::default());
    push_bounded(
        &entries,
        LogEntry {
            level: "error".into(),
            payload: "offline".into(),
            timestamp_ms: 1,
            ..LogEntry::default()
        },
    );

    push_bounded(
        &entries,
        LogEntry {
            level: "error".into(),
            payload: "offline".into(),
            timestamp_ms: 2,
            ..LogEntry::default()
        },
    );

    assert_eq!(
        entries
            .read()
            .entries
            .iter()
            .map(|entry| entry.entry.as_ref().clone())
            .collect::<Vec<_>>(),
        vec![LogEntry {
            level: "error".into(),
            payload: "offline".into(),
            timestamp_ms: 2,
            ..LogEntry::default()
        }]
    );
}

#[test]
fn bounded_log_buffer_discards_the_oldest_entry() {
    let entries = RwLock::new(LogBuffer::default());
    for index in 0..MAX_LOG_ENTRIES {
        push_bounded(
            &entries,
            LogEntry {
                payload: index.to_string(),
                ..LogEntry::default()
            },
        );
    }

    push_bounded(
        &entries,
        LogEntry {
            payload: "latest".into(),
            ..LogEntry::default()
        },
    );

    let entries = entries.read();
    assert_eq!(entries.entries.len(), MAX_LOG_ENTRIES);
    assert_eq!(
        entries
            .entries
            .front()
            .map(|entry| entry.entry.payload.as_str()),
        Some("1")
    );
    assert_eq!(
        entries
            .entries
            .back()
            .map(|entry| entry.entry.payload.as_str()),
        Some("latest")
    );
    drop(entries);
}

#[test]
fn log_buffer_byte_budget_keeps_the_newest_entries() {
    let entries = RwLock::new(LogBuffer::default());
    for index in 0..500 {
        push_bounded(
            &entries,
            LogEntry {
                payload: format!("{index:03}:{}", "x".repeat(16 * 1024)),
                ..LogEntry::default()
            },
        );
    }
    let entries = entries.read();
    let bytes: usize = entries
        .entries
        .iter()
        .map(|entry| log_entry_bytes(entry.entry.as_ref()))
        .sum();
    assert!(bytes <= MAX_LOG_BUFFER_BYTES);
    assert_eq!(entries.bytes, bytes);
    assert!(
        entries
            .entries
            .back()
            .unwrap()
            .entry
            .payload
            .starts_with("499:")
    );
}

#[test]
fn log_buffer_byte_budget_rejects_one_oversized_entry() {
    let entries = RwLock::new(LogBuffer::default());
    push_bounded(
        &entries,
        LogEntry {
            payload: "x".repeat(128 * 1024),
            ..LogEntry::default()
        },
    );
    assert!(entries.read().entries.is_empty());
}

#[test]
fn oversized_frames_produce_a_bounded_diagnostic_without_advancing_accepted_time() {
    let entries = RwLock::new(LogBuffer::default());
    let stream = RwLock::new(LogStreamSnapshot {
        generation: 7,
        connected: true,
        updated_at_ms: 123,
        ..LogStreamSnapshot::default()
    });
    let revision = AtomicU64::new(0);
    let generation = AtomicU64::new(7);
    let file = file::LogFileWorker::start();
    for timestamp in [456, 789] {
        assert!(!accept_log_entry(
            &entries,
            &stream,
            &revision,
            &file.sender(),
            LogEntry {
                level: "info".into(),
                payload: "x".repeat(MAX_LOG_ENTRY_BYTES),
                received_at_ms: timestamp,
                ..LogEntry::default()
            },
            7,
            &generation,
        ));
    }

    let expected = zenclash_i18n::text("logs.errors.entry_too_large");
    let entries = entries.read();
    assert_eq!(entries.entries.len(), 1);
    assert_eq!(entries.entries[0].entry.payload, expected);
    assert!(entries.bytes < 1024);
    assert_eq!(stream.read().last_error.as_deref(), Some(expected.as_str()));
    assert_eq!(stream.read().updated_at_ms, 123);
    assert_eq!(revision.load(Ordering::Acquire), 2);
}

#[test]
fn log_levels_parse_mihomo_values_case_insensitively() {
    assert_eq!(
        MihomoLogLevel::from_api(" WARNING "),
        Some(MihomoLogLevel::Warning)
    );
    assert_eq!(MihomoLogLevel::from_api("trace"), None);
}

#[test]
fn late_log_frame_from_an_old_generation_is_rejected() {
    let entries = RwLock::new(LogBuffer::default());
    let stream = RwLock::new(LogStreamSnapshot {
        generation: 2,
        connected: true,
        ..LogStreamSnapshot::default()
    });
    let expected_generation = AtomicU64::new(2);
    let revision = AtomicU64::new(0);
    let file = file::LogFileWorker::start();

    let accepted = accept_log_entry(
        &entries,
        &stream,
        &revision,
        &file.sender(),
        LogEntry {
            level: "info".into(),
            payload: "old session".into(),
            timestamp_ms: 10,
            ..LogEntry::default()
        },
        1,
        &expected_generation,
    );

    assert!(!accepted);
    assert!(entries.read().entries.is_empty());
    assert_eq!(stream.read().generation, 2);
    assert_eq!(stream.read().updated_at_ms, 0);
    assert_eq!(revision.load(Ordering::Acquire), 0);
}

#[test]
fn stream_interruption_preserves_last_entry_time_and_format() {
    let stream = RwLock::new(LogStreamSnapshot {
        generation: 2,
        connected: true,
        updated_at_ms: 123,
        format: LogStreamFormat::Structured,
        ..LogStreamSnapshot::default()
    });
    let expected_generation = AtomicU64::new(2);

    assert!(update_log_connection(
        &stream,
        false,
        None,
        2,
        &expected_generation
    ));

    assert!(!stream.read().connected);
    assert_eq!(stream.read().updated_at_ms, 123);
    assert_eq!(stream.read().format, LogStreamFormat::Structured);
}

#[tokio::test]
async fn changing_log_level_updates_the_monitor_without_restarting_it() {
    let monitor = LogMonitor::start(
        &Handle::current(),
        MihomoEndpoint::default(),
        MihomoLogLevel::Info,
    );

    monitor.set_level(MihomoLogLevel::Warning);

    assert_eq!(monitor.level(), MihomoLogLevel::Warning);
    monitor.set_level(MihomoLogLevel::Debug);
    assert_eq!(monitor.level(), MihomoLogLevel::Debug);
}

#[test]
fn native_debug_stream_obeys_the_user_severity_threshold() {
    assert!(!log_level_accepts(MihomoLogLevel::Silent, "error"));
    assert!(log_level_accepts(MihomoLogLevel::Warning, "warn"));
    assert!(log_level_accepts(MihomoLogLevel::Warning, "error"));
    assert!(!log_level_accepts(MihomoLogLevel::Warning, "info"));
    assert!(!log_level_accepts(MihomoLogLevel::Info, "debug"));
    assert!(!log_level_accepts(MihomoLogLevel::Info, "unrecognized"));
}

#[tokio::test]
async fn pending_level_update_before_connect_does_not_trigger_redundant_reconnect() {
    use futures_util::SinkExt;
    use std::time::Duration;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let (level_tx, level_rx) = watch::channel(MihomoLogLevel::Info);
    level_tx.send_replace(MihomoLogLevel::Debug);
    let (_generation_tx, generation_rx) = watch::channel(1_u64);
    let entries = Arc::new(RwLock::new(LogBuffer::default()));
    let stream = Arc::new(RwLock::new(LogStreamSnapshot::default()));
    let revision = Arc::new(AtomicU64::new(0));
    let expected_generation = Arc::new(AtomicU64::new(1));
    let file = file::LogFileWorker::start();
    let state = LogMonitorState {
        entries: entries.clone(),
        stream,
        revision,
        expected_generation,
        file_sender: file.sender(),
    };

    let monitor = tokio::spawn(run_monitor(
        ControllerBinding::direct(endpoint),
        level_rx,
        generation_rx,
        state,
    ));

    let (tcp, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut socket = accept_async(tcp).await.unwrap();
    socket
        .send(Message::Text(
            r#"{"type":"debug","payload":"single-connect"}"#.into(),
        ))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        while entries.read().entries.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "level update already applied to the initial connection must not trigger a reconnect"
    );
    monitor.abort();
    drop(socket);
    drop(level_tx);
}
