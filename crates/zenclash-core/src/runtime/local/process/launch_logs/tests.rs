use super::*;

#[test]
fn oversized_output_is_discarded_until_the_next_line_and_eof_is_flushed() {
    let input = io::repeat(b'x')
        .take((MAX_LINE_BYTES * 32) as u64)
        .chain(io::Cursor::new(b"\nnext\r\nlast"));
    let mut lines = Vec::new();
    read_output(input, |line| lines.push(line.to_owned())).unwrap();
    assert_eq!(
        lines,
        vec!["x".repeat(MAX_LINE_BYTES), "next".into(), "last".into()]
    );
}

#[test]
fn unterminated_output_still_has_a_byte_limit() {
    let mut last = String::new();
    read_output(
        io::repeat(b'x').take((MAX_LINE_BYTES * 32) as u64),
        |line| last = line.to_owned(),
    )
    .unwrap();
    assert_eq!(last.len(), MAX_LINE_BYTES);
}

#[test]
fn history_enforces_byte_and_utf8_limits_before_its_line_count_limit() {
    let mut logs = LaunchLogHistory::default();
    let launch = logs.begin_launch();
    let long = "配".repeat(MAX_LINE_BYTES);
    for _ in 0..100 {
        logs.record(&launch, "CORE", &long);
    }
    assert!(logs.history.len() < super::super::MAX_LOG_LINES);
    assert!(logs.history.iter().all(|line| line.len() <= MAX_LINE_BYTES));
    assert_eq!(
        logs.bytes,
        logs.history.iter().map(|line| line.len()).sum::<usize>()
    );
    assert!(logs.bytes <= MAX_HISTORY_BYTES);
    assert_eq!(
        logs.current_last_line().unwrap(),
        **logs.history.back().unwrap()
    );
}

#[test]
fn controller_listener_conflict_is_detected_without_confusing_proxy_listener_errors() {
    let mut controller = LaunchLogHistory::default();
    let launch = controller.begin_launch();
    controller.record(&launch, "INFO", "External controller listen error: listen tcp 127.0.0.1:19191: bind: address already in use");
    let mut proxy = LaunchLogHistory::default();
    let launch = proxy.begin_launch();
    proxy.record(
        &launch,
        "CORE",
        "Start Mixed(http+socks) proxy listening at: 127.0.0.1:7890: address already in use",
    );

    assert!(controller.current_listener_error().is_some());
    assert!(proxy.current_listener_error().is_none());
}

#[test]
fn late_old_collector_output_keeps_history_without_changing_current_diagnostics() {
    let mut logs = LaunchLogHistory::default();
    let old = logs.begin_launch();
    logs.record(&old, "CORE", "old launch output");
    let current = logs.begin_launch();
    logs.record(&current, "INFO", "current startup");

    logs.record(
        &old,
        "CORE",
        "External controller listen error: address already in use",
    );

    assert!(logs.current_listener_error().is_none());
    assert_eq!(
        logs.current_last_line().as_deref(),
        Some("[INFO] current startup")
    );
    assert_eq!(logs.recent_messages().len(), 3);
    assert!(
        logs.recent_messages()
            .last()
            .unwrap()
            .contains("address already in use")
    );
}

#[test]
fn current_launch_errors_survive_display_history_eviction_and_reset_at_next_launch() {
    let mut logs = LaunchLogHistory::default();
    let old = logs.begin_launch();
    let current = logs.begin_launch();
    logs.record(
        &current,
        "CORE",
        "External controller listen error: address already in use",
    );
    for _ in 0..super::super::MAX_LOG_LINES {
        logs.record(&old, "INFO", "late historical output");
    }

    assert_eq!(logs.recent_messages().len(), super::super::MAX_LOG_LINES);
    assert!(
        logs.current_listener_error()
            .unwrap()
            .contains("address already in use")
    );
    logs.begin_launch();
    assert!(logs.current_listener_error().is_none());
    assert!(logs.current_last_line().is_none());
    assert_eq!(logs.recent_messages().len(), super::super::MAX_LOG_LINES);
}
