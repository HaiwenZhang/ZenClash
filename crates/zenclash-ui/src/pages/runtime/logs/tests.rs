use crate::pages::runtime::test_support::runtime_page_fixture::{
    runtime_page_services, runtime_page_test_directory, wait_for_page_state,
};
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::{Page, RuntimePage};
use super::export::{export_log_entries, prepare_log_payload};
use super::*;
use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn pausing_log_display_keeps_filterable_rows_and_resume_reads_the_monitor(
    cx: &mut gpui_kit::TestAppContext,
) {
    use super::super::test_support::{
        RuntimePageFixture as Fixture, open_runtime_page_window as open,
    };
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Logs);
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.persistent_loading && !page.logs.read(cx).loading
    });
    let entries = vec![
        log_fixture("frozen info"),
        Arc::new(zenclash_core::LogEntry {
            level: "warning".into(),
            payload: "frozen warning".into(),
            ..Default::default()
        }),
    ];
    let ids = entries
        .iter()
        .map(|entry| Arc::as_ptr(entry) as usize)
        .collect::<Vec<_>>();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.ui_visibility = super::super::lifecycle::UiVisibility::new(true);
            let revision = page.log_monitor.revision();
            page.logs.update(cx, |view, cx| {
                view.cancel_refresh();
                let mut presentation = LogPresentation::default();
                presentation.refresh(revision, String::new(), || entries);
                view.presentation = Arc::new(presentation);
                cx.notify();
            });
            cx.notify();
        });
        window.render_frame(cx);
        window.click("pause-logs-display", cx);
        window.click("clear-logs", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(page.read(cx).logs.read(cx).presentation.entries.len(), 2);
        assert!(page.read(cx).logs.read(cx).paused);
        window.click("log-display-order", cx);
        window.press("escape", cx);
        assert!(!page.read(cx).logs.read(cx).oldest_first);
        page.update(cx, |page, cx| {
            page.log_monitor.clear();
            page.log_monitor.set_level(MihomoLogLevel::Warning);
            page.update_log_presentation(cx);
        });
        window.render_frame(cx);
        window.find(("inspect-log", ids[0]));
        window.find(("inspect-log", ids[1]));
        window.click(
            (
                gpui_kit::ElementId::from("log-level-filter"),
                "WARNING".to_owned(),
            ),
            cx,
        );
        assert_eq!(
            page.read(cx).logs.read(cx).level_filter.as_deref(),
            Some("WARNING")
        );
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.logs.read(cx).loading
            && page.logs.read(cx).presentation.level_filter.as_deref() == Some("WARNING")
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("inspect-log", ids[0])).is_none());
        window.find(("inspect-log", ids[1]));
        assert!(page.read(cx).logs.read(cx).paused);
        window.click("pause-logs-display", cx);
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| !page.logs.read(cx).loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!page.read(cx).logs.read(cx).paused);
        assert!(window.try_find(("inspect-log", ids[1])).is_none());
        page.update(cx, |page, cx| {
            page.logs.update(cx, |view, cx| view.release_results(cx))
        });
        assert!(!page.read(cx).logs.read(cx).paused);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn settings_log_preferences_save_updates_owner_and_preserves_other_preferences(
    cx: &mut gpui_kit::TestAppContext,
) {
    log_preferences_from_settings(cx, false);
}

#[gpui_kit::test]
fn settings_log_preferences_save_failure_preserves_owner_and_releases_busy_state(
    cx: &mut gpui_kit::TestAppContext,
) {
    log_preferences_from_settings(cx, true);
}

fn log_preferences_from_settings(cx: &mut gpui_kit::TestAppContext, fail: bool) {
    use crate::pages::runtime::busy::MutationDomain;
    use zenclash_core::{AppPreferences, AppPreferencesStore, AppearancePreference};
    let directory = runtime_page_test_directory("log");
    let preference_path = directory.join("preferences.json");
    if fail {
        std::fs::create_dir(&preference_path).unwrap();
    }
    let store = AppPreferencesStore::new(preference_path);
    let preferences = AppPreferences {
        appearance: AppearancePreference::Dark,
        log_file_enabled: false,
        ..Default::default()
    };
    if !fail {
        store.update(|saved| *saved = preferences.clone()).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let services = runtime_page_services(&runtime, &directory, &store, preferences);
    let logs = services.log_monitor.clone();
    let status = services.operational_status.clone();
    cx.executor().allow_parking();
    cx.update(gpui_kit::init);
    let mut owner = None;
    let window = cx.open_window(
        gpui_kit::size(gpui_kit::px(1200.), gpui_kit::px(2600.)),
        |window, cx| {
            let page = cx.new(|cx| RuntimePage::new(Page::Settings, services, window, cx));
            owner = Some(page.clone());
            gpui_kit::component::Root::new(page, window, cx)
        },
    );
    let page = owner.unwrap();
    cx.foreground_executor().clone().block_test(async {
        wait_for_page_state(cx, &runtime, &page, |page| !page.persistent_loading).await;
        cx.update(|cx| {
            page.update(cx, |page, _| {
                page.error = None;
            })
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("settings-log-file-enabled", cx);
        })
        .unwrap();
        assert!(cx.update(|cx| page.read(cx).mutation_busy(MutationDomain::Logs)));
        wait_for_page_state(cx, &runtime, &page, |page| {
            !page.mutation_busy(MutationDomain::Logs)
        })
        .await;
    });
    cx.update(|cx| {
        let current = page.read(cx);
        assert!(!current.mutation_busy(MutationDomain::Logs));
        assert_eq!(current.page, Page::Settings);
        assert_eq!(current.preferences.appearance, AppearancePreference::Dark);
        assert_eq!(current.preferences.log_file_enabled, !fail);
        if fail {
            assert!(current.error.is_some());
            assert!(!logs.persistence_status().enabled);
        } else {
            assert!(current.error.is_none());
            assert!(store.load().unwrap().log_file_enabled);
            assert!(logs.persistence_status().enabled);
        }
    });
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    status.stop();
    runtime.shutdown_timeout(Duration::from_secs(1));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn level_filter_combines_with_search_and_counts_the_whole_buffer() {
    let warning = Arc::new(zenclash_core::LogEntry {
        level: "warn".into(),
        payload: "network timeout".into(),
        ..Default::default()
    });
    let info = Arc::new(zenclash_core::LogEntry {
        level: "INFO".into(),
        payload: "network connected".into(),
        ..Default::default()
    });
    let mut presentation = LogPresentation::default();
    presentation.refresh(1, "network".into(), || vec![warning, info]);
    presentation.set_level_filter(Some("WARNING".into()));
    assert_eq!(presentation.matches, [0]);
    assert_eq!(
        presentation.level_counts,
        [("INFO".into(), 1), ("WARNING".into(), 1)]
    );
    presentation.refresh(1, "connected".into(), || {
        panic!("search fetched another snapshot")
    });
    assert!(presentation.matches.is_empty());
    presentation.set_level_filter(None);
    assert_eq!(presentation.matches, [1]);
}

#[tokio::test]
async fn changing_only_level_filter_reuses_snapshot_and_updates_visible_entries() {
    let mut worker = LogProjectionWorker::default();
    let (_, task) = worker.start(
        &tokio::runtime::Handle::current(),
        Arc::default(),
        String::new(),
        || {
            snapshot(
                1,
                Some(vec![
                    Arc::new(zenclash_core::LogEntry {
                        level: "debug".into(),
                        ..Default::default()
                    }),
                    Arc::new(zenclash_core::LogEntry {
                        level: "info".into(),
                        ..Default::default()
                    }),
                ]),
            )
        },
    );
    let previous = task.await.unwrap().unwrap().unwrap().presentation;
    let (_, task) = worker.start_filtered(
        &tokio::runtime::Handle::current(),
        previous,
        String::new(),
        Some("DEBUG".into()),
        false,
        || snapshot(1, None),
    );
    let filtered = task.await.unwrap().unwrap().unwrap().presentation;
    assert_eq!(filtered.matches, [0]);
    assert_eq!(filtered.entries.len(), 2);
}

fn log_fixture(payload: &str) -> Arc<zenclash_core::LogEntry> {
    Arc::new(zenclash_core::LogEntry {
        payload: payload.into(),
        ..Default::default()
    })
}

fn snapshot(revision: u64, entries: Option<Vec<Arc<zenclash_core::LogEntry>>>) -> LogSnapshot {
    LogSnapshot {
        revision,
        entries,
        connected: true,
        level: MihomoLogLevel::Info,
        persistence: zenclash_core::LogPersistenceStatus::default(),
        pending_entries: 0,
    }
}

#[tokio::test]
async fn cancelling_a_pending_log_read_never_reads_the_source() {
    let reads = Arc::new(AtomicU64::new(0));
    let source_reads = reads.clone();
    let mut worker = LogProjectionWorker::default();
    let (generation, task) = worker.start(
        &tokio::runtime::Handle::current(),
        Arc::default(),
        String::new(),
        move || {
            source_reads.fetch_add(1, Ordering::AcqRel);
            snapshot(1, Some(vec![log_fixture("cancelled")]))
        },
    );
    worker.cancel();

    assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
    assert!(!worker.is_current(generation));
    assert_eq!(reads.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn a_blocked_log_source_does_not_block_the_request_executor_or_publish_after_cancellation() {
    let caller_thread = std::thread::current().id();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let mut worker = LogProjectionWorker::default();
    let (old, task) = worker.start(
        &tokio::runtime::Handle::current(),
        Arc::default(),
        "old".into(),
        move || {
            started_sender.send(std::thread::current().id()).unwrap();
            release_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            snapshot(1, Some(vec![log_fixture("old")]))
        },
    );
    let source_thread = tokio::time::timeout(Duration::from_secs(1), started_receiver)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(source_thread, caller_thread);
    worker.cancel();
    assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
    let (latest, task) = worker.start(
        &tokio::runtime::Handle::current(),
        Arc::default(),
        "latest".into(),
        || snapshot(2, Some(vec![log_fixture("old"), log_fixture("latest")])),
    );
    release_sender.send(()).unwrap();
    let view = task.await.unwrap().unwrap().unwrap();

    assert!(!worker.is_current(old));
    assert!(worker.is_current(latest));
    assert_eq!(view.presentation.matches, [1]);
    assert_eq!(view.presentation.rows[1].payload.as_ref(), "latest");
}

#[tokio::test]
async fn searching_cached_logs_and_refreshing_health_do_not_need_another_entry_snapshot() {
    let mut worker = LogProjectionWorker::default();
    let runtime = tokio::runtime::Handle::current();
    let (_, task) = worker.start(&runtime, Arc::default(), String::new(), || {
        snapshot(1, Some(vec![log_fixture("old"), log_fixture("new")]))
    });
    let original = task.await.unwrap().unwrap().unwrap().presentation;
    let (_, task) = worker.start(&runtime, original.clone(), "old".into(), || {
        snapshot(1, None)
    });
    let filtered = task.await.unwrap().unwrap().unwrap().presentation;
    assert_eq!(filtered.matches, [0]);
    assert_eq!(filtered.entries.len(), 2);

    let (_, task) = worker.start(&runtime, filtered.clone(), "old".into(), || {
        let mut source = snapshot(1, None);
        source.connected = false;
        source.persistence.dropped_entries = 3;
        source
    });
    let health = task.await.unwrap().unwrap().unwrap();
    assert!(Arc::ptr_eq(&filtered, &health.presentation));
    assert!(!health.connected);
    assert_eq!(health.persistence.dropped_entries, 3);
    assert_eq!(health.presentation.matches, [0]);
}

#[test]
fn prepared_log_rows_keep_the_full_export_source_and_structured_fields() {
    let entry = Arc::new(zenclash_core::LogEntry {
        level: "warning".into(),
        payload: "prepared payload".into(),
        fields: serde_json::json!({"detail": "界".repeat(300)}),
        ..Default::default()
    });
    let mut presentation = LogPresentation::default();
    presentation.refresh(1, String::new(), || vec![entry.clone()]);

    assert_eq!(presentation.rows[0].level.as_ref(), "WARNING");
    assert_eq!(presentation.rows[0].payload.as_ref(), "prepared payload");
    assert_eq!(presentation.entries[0].fields, entry.fields);
    assert!(Arc::ptr_eq(&presentation.entries[0], &entry));
    assert_eq!(
        presentation.entries[0].fields["detail"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        300
    );
}

#[tokio::test]
async fn export_reports_write_failure_and_next_export_preserves_snapshot() {
    let directory = std::env::temp_dir().join(format!(
        "zenclash-log-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let entries = vec![log_fixture("first entry"), log_fixture("second entry")];
    let expected = prepare_log_payload(entries.clone(), false);
    let failed = export_log_entries(directory.clone(), entries.clone()).await;
    let path = directory.join("export.log");
    let saved = export_log_entries(path.clone(), entries).await;
    let content = tokio::fs::read_to_string(&path).await;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777
    };
    std::fs::remove_dir_all(directory).unwrap();

    assert!(failed.is_err(), "writing a directory must report an error");
    saved.unwrap();
    assert_eq!(content.unwrap(), expected);
    #[cfg(unix)]
    assert_eq!(permissions, 0o600);
}

#[test]
fn background_payload_preserves_export_and_redacts_support_copy() {
    let entries = vec![
        log_fixture("private-target.example"),
        log_fixture("second entry"),
    ];
    let raw = prepare_log_payload(entries.clone(), false);
    let safe = prepare_log_payload(entries, true);
    assert!(raw.find("private-target.example").unwrap() < raw.find("second entry").unwrap());
    assert!(!safe.contains("private-target.example"));
    assert!(!safe.contains("second entry"));
    assert_eq!(safe.lines().count(), 2);
}

#[test]
fn redraw_and_search_reuse_the_same_log_snapshot() {
    let mut presentation = LogPresentation::default();
    presentation.refresh(1, String::new(), || {
        vec![log_fixture("old"), log_fixture("new")]
    });
    assert_eq!(presentation.matches, [1, 0]);
    presentation.refresh(1, String::new(), || {
        panic!("redraw fetched another snapshot")
    });
    presentation.refresh(1, "old".into(), || {
        panic!("search fetched another snapshot")
    });
    assert_eq!(presentation.matches, [0]);
}

#[test]
fn chronological_order_survives_filters_and_new_snapshots() {
    let mut presentation = LogPresentation::default();
    presentation.refresh(1, String::new(), || {
        vec![log_fixture("old"), log_fixture("new")]
    });
    presentation.set_order(true);
    assert_eq!(presentation.matches, [0, 1]);
    presentation.set_level_filter(Some(String::new()));
    assert_eq!(presentation.matches, [0, 1]);
    presentation.refresh(2, String::new(), || {
        vec![
            log_fixture("old"),
            log_fixture("new"),
            log_fixture("latest"),
        ]
    });
    assert_eq!(presentation.matches, [0, 1, 2]);
    presentation.set_order(false);
    assert_eq!(presentation.matches, [2, 1, 0]);
}

#[test]
fn log_rows_normalize_time_and_warning() {
    let mut entry = zenclash_core::LogEntry {
        level: "warn".into(),
        core_time: Some("12:34:56".into()),
        ..Default::default()
    };
    let row = LogRow::from(&entry);
    assert_eq!(row.level.as_ref(), "WARNING");
    assert_eq!(row.time.as_ref(), "12:34:56");
    entry.core_time = Some("invalid".into());
    entry.timestamp_ms = u64::MAX;
    assert_eq!(format_log_time(&entry), "—");
    entry.timestamp_ms = 1_728_000_000_000;
    let expected = chrono::DateTime::from_timestamp_millis(1_728_000_000_000)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string();
    assert_eq!(format_log_time(&entry), expected);
}

#[test]
fn log_rotation_and_clear_replace_filtered_indices() {
    let mut presentation = LogPresentation::default();
    presentation.refresh(1, "match".into(), || {
        vec![log_fixture("match"), log_fixture("other")]
    });
    assert_eq!(presentation.matches, [0]);
    presentation.refresh(2, "match".into(), || {
        vec![log_fixture("other"), log_fixture("match new")]
    });
    assert_eq!(presentation.matches, [1]);
    presentation.refresh(3, "match".into(), Vec::new);
    assert!(presentation.entries.is_empty());
    assert!(presentation.matches.is_empty());
}

#[test]
fn log_filter_matches_level_and_payload_case_insensitively() {
    let entry = zenclash_core::LogEntry {
        level: "warning".into(),
        payload: "Proxy connection timeout".into(),
        timestamp_ms: 0,
        ..zenclash_core::LogEntry::default()
    };

    assert!(log_matches(&entry, &normalize_log_query("WARN")));
    assert!(log_matches(
        &entry,
        &normalize_log_query(" PROXY connection ")
    ));
    assert!(!log_matches(&entry, "dns"));
}

#[test]
fn log_filter_matches_structured_field_names_and_values() {
    let entry = zenclash_core::LogEntry {
        fields: serde_json::json!({"network": "tcp"}),
        ..zenclash_core::LogEntry::default()
    };

    assert!(log_matches(&entry, "network"));
    assert!(log_matches(&entry, "tcp"));
    assert!(!log_matches(&entry, "udp"));
}

#[test]
fn log_level_descriptions_distinguish_daily_use_from_diagnostics() {
    let warning = log_level_description(MihomoLogLevel::Warning);
    let debug = log_level_description(MihomoLogLevel::Debug);
    assert!(!warning.is_empty());
    assert!(!debug.is_empty());
    assert_ne!(warning, debug);
}

#[gpui_kit::test]
fn hiding_logs_cancels_a_pending_projection_and_restores_the_current_filter(
    cx: &mut gpui_kit::TestAppContext,
) {
    exercise_interrupted_log_projection(cx, LogDisplayInterruption::WindowHidden);
}

#[gpui_kit::test]
fn leaving_logs_cancels_a_pending_projection_and_restores_the_current_filter(
    cx: &mut gpui_kit::TestAppContext,
) {
    exercise_interrupted_log_projection(cx, LogDisplayInterruption::NavigationAway);
}

enum LogDisplayInterruption {
    WindowHidden,
    NavigationAway,
}

fn exercise_interrupted_log_projection(
    cx: &mut gpui_kit::TestAppContext,
    interruption: LogDisplayInterruption,
) {
    use super::super::test_support::{
        RuntimePageFixture as Fixture, open_runtime_page_window as open,
    };
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Logs);
    cx.update_window(window, |_, window, cx| {
        window.activate_window();
        page.update(cx, |page, cx| {
            page.ui_visibility = super::super::lifecycle::UiVisibility::new(true);
            page.update_log_presentation(cx);
            cx.notify();
        });
    })
    .unwrap();
    fixture.settle_with_context(cx, &page, |page, cx| {
        !page.persistent_loading
            && !page.logs.read(cx).loading
            && page.logs.read(cx).presentation.revision.is_some()
    });
    let gate = cx.update(|cx| page.read(cx).logs.read(cx).worker.gate.clone());
    let blocked_projection = gate.try_lock_owned().unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let filter_id = page.read(cx).logs.read(cx).filter.entity_id();
        window.click(("input", filter_id), cx);
        window.input("obsolete query", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let obsolete_generation = cx.update(|cx| {
        let view = page.read(cx).logs.read(cx);
        assert!(view.loading);
        assert_eq!(view.query, "obsolete query");
        view.worker.generation.load(Ordering::Acquire)
    });
    cx.update(|cx| {
        page.update(cx, |page, cx| match interruption {
            LogDisplayInterruption::WindowHidden => page.set_window_visible(false, cx),
            LogDisplayInterruption::NavigationAway => page.switch_to(Page::Settings, cx),
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let view = page.read(cx).logs.read(cx);
        assert!(!view.loading);
        assert!(!view.worker.is_current(obsolete_generation));
        assert!(view.presentation.revision.is_none());
        assert!(view.presentation.entries.is_empty());
        assert!(!view.context.unwrap().active);
    });
    let monitor = cx.update(|cx| page.read(cx).log_monitor.clone());
    monitor.synchronize_generation(7001);
    let refreshed_revision = monitor.revision();
    cx.update(|cx| {
        page.update(cx, |page, cx| match interruption {
            LogDisplayInterruption::WindowHidden => page.set_window_visible(true, cx),
            LogDisplayInterruption::NavigationAway => page.switch_to(Page::Logs, cx),
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let filter_id = page.read(cx).logs.read(cx).filter.entity_id();
        window.click(("input", filter_id), cx);
        window.press("secondary-a", cx);
        window.input("current query", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let view = page.read(cx).logs.read(cx);
        assert_eq!(view.query, "current query");
        assert!(view.loading);
    });
    drop(blocked_projection);
    fixture.settle_with_context(cx, &page, |page, cx| {
        let view = page.logs.read(cx);
        !view.loading
            && view.presentation.query == "current query"
            && view
                .presentation
                .revision
                .is_some_and(|revision| revision >= refreshed_revision)
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let view = page.read(cx).logs.read(cx);
        assert!(!view.worker.is_current(obsolete_generation));
        assert!(view.context.unwrap().active);
        assert_eq!(view.presentation.query, "current query");
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_log_copy_request_from_before_navigation_round_trip_is_discarded(
    cx: &mut gpui_kit::TestAppContext,
) {
    use super::super::test_support::{
        RuntimePageFixture as Fixture, open_runtime_page_window as open,
    };
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Logs);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("existing clipboard".into()));
        page.update(cx, |page, cx| {
            let submitted = page.page_task_token_for(Page::Logs);
            page.logs
                .update(cx, |view, cx| view.request_support_copy(cx));
            assert!(page.logs.read(cx).copying);
            page.switch_to(Page::Settings, cx);
            page.switch_to(Page::Logs, cx);
            assert!(!page.is_page_task_current(submitted));
        });
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            !page.read(cx).logs.read(cx).copying,
            "the rejected request must release its original view's busy state"
        );
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("existing clipboard")
        );
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
