use super::*;

const SOURCE: &str = "history.sqlite3";

fn completed_state() -> TrafficHistoryQueryState {
    let mut state = TrafficHistoryQueryState::default();
    let request = state.begin_query(PathBuf::from(SOURCE), 5_000);
    let mut snapshot = TrafficHistorySnapshot::default();
    snapshot.overview.totals.total = 99;
    state.complete_query(&request, Some(Path::new(SOURCE)), Ok(snapshot), 5_000);
    state
}

#[test]
fn an_old_completion_cannot_change_the_loading_or_error_of_a_reopened_page() {
    for result in [
        Ok(TrafficHistorySnapshot::default()),
        Err("old query failed".into()),
    ] {
        let mut state = TrafficHistoryQueryState::default();
        let old = state.begin_query(PathBuf::from(SOURCE), 1_000);
        state.release_results();
        let current = state.begin_query(PathBuf::from(SOURCE), 2_000);
        assert!(!state.complete_query(&old, Some(Path::new(SOURCE)), result, 1_000));
        assert!(state.loading);
        assert!(state.last_error.is_none());
        assert!(state.completed_query().is_none());
        state.complete_query(
            &current,
            Some(Path::new(SOURCE)),
            Ok(TrafficHistorySnapshot::default()),
            2_000,
        );
        assert!(!state.loading);
        assert_eq!(state.completed_query().unwrap().observed_at_ms(), 2_000);
    }
}

#[test]
fn cancelling_a_hidden_query_prevents_publishing_or_requesting_another_query() {
    let mut state = TrafficHistoryQueryState::default();
    let token = state.begin_query(PathBuf::from(SOURCE), 1_000);
    state.change_range(TrafficRange::Week);
    state.cancel_query();
    assert!(!state.complete_query(
        &token,
        Some(Path::new(SOURCE)),
        Err("hidden query failed".into()),
        1_000
    ));
    assert!(!state.loading);
    assert!(state.last_error.is_none());
}

#[test]
fn changing_query_selection_requests_one_refresh_without_publishing_old_data() {
    let mut state = TrafficHistoryQueryState::default();
    let token = state.begin_query(PathBuf::from(SOURCE), 1_000);
    state.change_dimension(TrafficDimension::Process);
    assert!(state.complete_query(
        &token,
        Some(Path::new(SOURCE)),
        Err("obsolete selection failed".into()),
        1_000
    ));
    assert!(state.last_error.is_none());
    assert!(state.completed_query().is_none());
    assert!(!state.complete_query(
        &token,
        Some(Path::new(SOURCE)),
        Err("duplicate completion".into()),
        2_000
    ));
}

#[test]
fn history_ranges_produce_the_expected_bounded_bucket_counts() {
    for (range, expected_count) in TrafficRange::ALL.into_iter().zip([12_u64, 24, 28, 30]) {
        let mut state = TrafficHistoryQueryState {
            range,
            ..Default::default()
        };
        let request = state.begin_query(PathBuf::from(SOURCE), range.duration_ms() * 2);
        let plan = request.plan();
        let query = plan.query();
        assert_eq!(
            (query.end_ms - query.start_ms) / query.bucket_ms + 1,
            expected_count
        );
    }
}

#[test]
fn replacing_the_history_source_discards_old_results_and_allows_a_fresh_query() {
    let mut state = completed_state();
    let old = state.begin_query(PathBuf::from(SOURCE), 6_000);
    assert!(state.complete_query(
        &old,
        Some(Path::new("new.sqlite3")),
        Ok(TrafficHistorySnapshot::default()),
        6_000
    ));
    assert!(
        state
            .presentation_for_source(Some(Path::new("new.sqlite3")))
            .is_none()
    );
    assert_eq!(
        state.freshness(Some(Path::new("new.sqlite3"))),
        TrafficHistoryFreshness::Loading
    );
    let current = state.begin_query(PathBuf::from("new.sqlite3"), 7_000);
    assert!(!state.complete_query(
        &old,
        Some(Path::new("new.sqlite3")),
        Err("late old-source failure".into()),
        6_000
    ));
    assert!(state.loading);
    state.complete_query(
        &current,
        Some(Path::new("new.sqlite3")),
        Ok(TrafficHistorySnapshot::default()),
        7_000,
    );
    assert_eq!(
        state
            .presentation_for_source(Some(Path::new("new.sqlite3")))
            .unwrap()
            .observed_at_ms(),
        7_000
    );
}

#[test]
fn every_traffic_dimension_has_a_distinct_user_label() {
    let labels = [
        TrafficDimension::Host,
        TrafficDimension::SourceIp,
        TrafficDimension::Outbound,
        TrafficDimension::Process,
    ]
    .map(dimension_label);
    assert!(labels.iter().all(|label| !label.is_empty()));
    assert_eq!(
        labels
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
}

#[test]
fn historical_data_becomes_stale_instead_of_disappearing_after_failure() {
    let mut state = completed_state();
    let request = state.begin_query(PathBuf::from(SOURCE), 6_000);
    state.complete_query(
        &request,
        Some(Path::new(SOURCE)),
        Err("database busy".into()),
        6_000,
    );
    assert_eq!(
        state.freshness(Some(Path::new(SOURCE))),
        TrafficHistoryFreshness::Stale {
            observed_at_ms: 5_000
        }
    );
    assert_eq!(state.completed_query().unwrap().overview().totals.total, 99);
}

#[test]
fn leaving_traffic_releases_derived_results_but_keeps_the_user_selection() {
    let mut state = completed_state();
    state.change_range(TrafficRange::Week);
    state.change_dimension(TrafficDimension::Outbound);
    state.release_results();
    assert_eq!(state.range, TrafficRange::Week);
    assert_eq!(state.dimension, TrafficDimension::Outbound);
    assert!(state.completed_query().is_none());
    assert_eq!(state.selected_parent, None);
    assert_eq!(state.selected_detail, None);
}

#[test]
fn completed_facts_keep_their_query_and_cannot_be_inspected_under_new_controls() {
    let mut state = completed_state();
    let presentation = state.completed_query().unwrap();
    assert_eq!(presentation.range(), TrafficRange::Day);
    assert_eq!(presentation.dimension(), TrafficDimension::Host);
    state.change_range(TrafficRange::Week);
    state.change_dimension(TrafficDimension::Process);
    assert!(!state.select_parent("old host".into(), Some(Path::new(SOURCE))));
    let request = state.begin_query(PathBuf::from(SOURCE), 6_000);
    let plan = request.plan();
    assert_eq!(plan.query().dimension, TrafficDimension::Process);
    state.complete_query(
        &request,
        Some(Path::new(SOURCE)),
        Err("new query failed".into()),
        6_000,
    );
    let presentation = state.completed_query().unwrap();
    assert_eq!(presentation.range(), TrafficRange::Day);
    assert_eq!(presentation.dimension(), TrafficDimension::Host);
    assert_eq!(presentation.overview().totals.total, 99);
}

#[test]
fn inspector_labels_and_all_payload_levels_follow_the_completed_selection() {
    let mut state = completed_state();
    assert!(!state.select_detail("orphan detail".into(), Some(Path::new(SOURCE))));
    assert!(state.select_parent("parent".into(), Some(Path::new(SOURCE))));
    let parent_query = state.begin_query(PathBuf::from(SOURCE), 6_000);
    state.complete_query(
        &parent_query,
        Some(Path::new(SOURCE)),
        Ok(TrafficHistorySnapshot::default()),
        6_000,
    );
    assert!(state.select_detail("detail".into(), Some(Path::new(SOURCE))));
    let detail_query = state.begin_query(PathBuf::from(SOURCE), 7_000);
    let mut snapshot = TrafficHistorySnapshot::default();
    snapshot.details = vec![zenclash_core::TrafficAggregate {
        label: "detail".into(),
        total: 5,
        ..Default::default()
    }];
    snapshot.proxy_stats = vec![zenclash_core::TrafficAggregate {
        label: "outlet".into(),
        total: 5,
        ..Default::default()
    }];
    state.complete_query(&detail_query, Some(Path::new(SOURCE)), Ok(snapshot), 7_000);
    assert!(state.select_parent("new parent".into(), Some(Path::new(SOURCE))));
    let presentation = state.completed_query().unwrap();
    assert_eq!(presentation.selected_parent(), Some("parent"));
    assert_eq!(presentation.selected_detail(), Some("detail"));
    assert_eq!(presentation.details()[0].label, "detail");
    assert_eq!(presentation.proxy_stats()[0].label, "outlet");
    assert!(!state.select_detail("old detail".into(), Some(Path::new(SOURCE))));
}

use crate::pages::runtime::test_support::runtime_page_fixture::{
    runtime_page_services, runtime_page_test_directory, wait_for_page_state,
};
use crate::pages::runtime::{Page, RuntimePage};
use gpui_kit::AppContext;
use gpui_kit::test::TestWindowExt;
use std::time::Duration;

#[gpui_kit::test]
fn settings_clear_history_removes_real_records_and_resets_confirmation(
    cx: &mut gpui_kit::TestAppContext,
) {
    clear_history_from_settings(cx, false);
}

#[gpui_kit::test]
fn settings_clear_history_failure_keeps_totals_and_releases_busy_state(
    cx: &mut gpui_kit::TestAppContext,
) {
    clear_history_from_settings(cx, true);
}

fn clear_history_from_settings(cx: &mut gpui_kit::TestAppContext, fail: bool) {
    use crate::pages::runtime::busy::MutationDomain;
    use zenclash_core::{
        AppPreferences, AppPreferencesStore, TrafficDimension, TrafficHistoryEntry,
        TrafficHistoryQuery, TrafficHistoryStore,
    };
    let directory = runtime_page_test_directory("clear");
    let history = TrafficHistoryStore::new(directory.join("history.sqlite"));
    history
        .insert_and_cleanup(
            &[TrafficHistoryEntry {
                timestamp_ms: 1000,
                source_ip: "127.0.0.1".into(),
                host: "example.test".into(),
                outbound: "DIRECT".into(),
                process: "test".into(),
                upload: 10,
                download: 20,
            }],
            0,
        )
        .unwrap();
    let query = TrafficHistoryQuery {
        dimension: TrafficDimension::Host,
        start_ms: 0,
        end_ms: 2000,
        bucket_ms: 1000,
    };
    let overview = history.overview(&query).unwrap();
    assert_eq!(overview.totals.total, 30);
    let store = AppPreferencesStore::new(directory.join("preferences.json"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let services = runtime_page_services(&runtime, &directory, &store, AppPreferences::default());
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
                page.traffic_history.replace_store_for_test(history.clone());
                let request = page
                    .traffic_history
                    .state
                    .begin_query(history.path().to_path_buf(), 2_000);
                let plan = request.plan();
                let snapshot = history.query(plan.query(), plan.selection()).unwrap();
                page.traffic_history.state.complete_query(
                    &request,
                    Some(history.path()),
                    Ok(snapshot),
                    2_000,
                );
            });
        });
        if fail {
            std::fs::remove_file(history.path()).unwrap();
            std::fs::create_dir(history.path()).unwrap();
        }
        cx.update(|cx| {
            page.update(cx, |page, _| {
                page.error = None;
                page.traffic_history.replace_store_for_test(history.clone());
                page.traffic_history.request_clear_confirmation();
            })
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("confirm-clear-traffic", cx);
        })
        .unwrap();
        assert!(cx.update(|cx| page.read(cx).mutation_busy(MutationDomain::TrafficHistory)));
        wait_for_page_state(cx, &runtime, &page, |page| {
            !page.mutation_busy(MutationDomain::TrafficHistory)
        })
        .await;
    });
    cx.update(|cx| {
        let current = page.read(cx);
        assert_eq!(current.page, Page::Settings);
        assert!(!current.traffic_history.is_clear_confirmation_open());
        assert!(!current.mutation_busy(MutationDomain::TrafficHistory));
        if fail {
            assert!(current.error.is_some());
            assert_eq!(
                current
                    .traffic_history
                    .completed_query()
                    .map_or(0, |presentation| presentation.overview().totals.total),
                30
            );
        } else {
            assert!(current.error.is_none());
            assert_eq!(
                current
                    .traffic_history
                    .completed_query()
                    .map_or(0, |presentation| presentation.overview().totals.total),
                0
            );
            assert_eq!(history.overview(&query).unwrap().totals.samples, 0);
        }
    });
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    status.stop();
    runtime.shutdown_timeout(Duration::from_secs(1));
    let _ = std::fs::remove_dir_all(directory);
}
