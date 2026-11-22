use super::*;

fn frame(time: u64) -> TrafficSnapshot {
    TrafficSnapshot {
        generation: 1,
        updated_at_ms: time,
        upload: time,
        download: time * 2,
        ..TrafficSnapshot::default()
    }
}

#[test]
fn metric_history_accepts_only_new_real_observations_and_releases_old_generation() {
    let mut state = HomeChartState::default();
    assert!(state.observe(&frame(1_000), 1));
    assert!(!state.observe(&frame(1_000), 1));
    assert_eq!(state.sparkline(0, 1).len(), 1);
    assert_eq!(state.sparkline(0, 1)[0].1, 2_000.);
    assert_eq!(state.sparkline(1, 1)[0].1, 1_000.);
    assert!(state.sparkline(0, 2).is_empty());
    assert!(state.observe(&frame(3_000), 2));
    assert_eq!(state.sparkline(0, 2).len(), 1);
}

#[test]
fn short_history_occupies_only_its_observed_part_of_the_two_minute_frame() {
    assert_eq!(observed_span_fraction(60), 0.5);
    assert_eq!(observed_span_fraction(120), 1.);
    assert_eq!(observed_span_fraction(600), 1.);
}

#[test]
fn chart_windows_use_actual_timestamps_and_do_not_duplicate_frames() {
    let mut state = HomeChartState::default();
    for seconds in 1..=400 {
        state.observe(&frame(seconds * 1_000), 1);
    }
    state.observe(&frame(400_000), 1);
    assert_eq!(state.samples.len(), 300);
    assert_eq!(state.points(1).0.len(), 121);
    assert_eq!(state.points(1).1, 120);
}

#[test]
fn configuration_change_preserves_history_but_rejects_stale_new_frames() {
    let mut state = HomeChartState::default();
    state.observe(&frame(1_000), 1);
    state.observe(&frame(2_000), 1);
    assert_eq!(state.points(1).0[0].upload, 1_000.);
    assert_eq!(state.samples.len(), 2);
    assert_eq!(state.points(1).0.len(), 2);
    state.observe(&frame(3_000), 2);
    assert_eq!(state.samples.len(), 2);
    assert!(state.points(1).0.is_empty());
    state.observe(
        &TrafficSnapshot {
            generation: 2,
            ..frame(4_000)
        },
        2,
    );
    assert_eq!(state.points(2).0.len(), 3);
    assert_eq!(state.points(2).0[0].upload, 1_000.);
}

#[test]
fn restoring_background_frames_preserves_timestamps_and_does_not_duplicate_history() {
    let sample = |time| {
        TimedTrafficSample::new(
            time,
            TrafficSample {
                upload: time,
                download: time * 2,
            },
        )
    };
    let mut state = HomeChartState::default();
    state.observe_samples(&VecDeque::from([sample(1_000)]), 1);
    let background = VecDeque::from([sample(1_000), sample(2_000), sample(5_000)]);
    assert!(state.observe_samples(&background, 1));
    assert!(!state.observe_samples(&background, 1));
    assert_eq!(state.points(1).1, 4);
    assert_eq!(
        state
            .sparkline(1, 1)
            .iter()
            .map(|(_, value)| *value)
            .collect::<Vec<_>>(),
        vec![1_000., 2_000., 5_000.]
    );
}

#[gpui_kit::test]
fn unfocused_visible_window_keeps_processing_live_traffic_revisions(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};
    use gpui_kit::{AppContext, VisualTestContext};

    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.wait_for_initial_reads(cx, &page);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    VisualTestContext::from_window(window, cx).deactivate_window();
    cx.update_window(window, |_, window, cx| {
        assert!(!window.is_window_active());
        assert!(page.read(cx).live_updates_enabled());
    })
    .unwrap();

    // A stream revision must reach the chart through its timer, without pointer input.
    let previous_revision = fixture.traffic.revision();
    fixture
        .traffic
        .synchronize_generation(fixture.core.generation().wrapping_add(1));
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        assert!(!window.is_window_active());
        assert!(page.read(cx).home.chart.traffic_revision.unwrap() > previous_revision);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn traffic_history_survives_page_switches_and_hidden_window_restoration(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};
    use gpui_kit::AppContext;
    use gpui_kit::test::TestWindowExt;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Home);
    fixture.wait_for_initial_reads(cx, &page);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let generation = page.core_session.generation();
            page.reconcile_home_generation();
            for time in [1_000, 2_000, 3_000] {
                page.home.chart.observe(
                    &TrafficSnapshot {
                        generation,
                        ..frame(time)
                    },
                    generation,
                );
            }
            page.switch_to(Page::Settings, cx);
            page.switch_to(Page::Home, cx);
            assert_eq!(page.home.chart.points(generation).0.len(), 3);
            page.set_window_visible(false, cx);
            page.set_window_visible(true, cx);
            assert_eq!(page.home.chart.points(generation).0.len(), 3);
            page.set_presented(false, cx);
            page.set_presented(true, cx);
            assert_eq!(page.home.chart.points(generation).0.len(), 3);
            assert_eq!(page.home.chart.points(generation).0[0].upload, 1_000.);
        });
        window.render_frame(cx);
        assert!(window.find("home-traffic-row").visible());
        window.remove_window();
    })
    .unwrap();
}
