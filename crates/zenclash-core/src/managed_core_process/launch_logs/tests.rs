use super::*;

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
