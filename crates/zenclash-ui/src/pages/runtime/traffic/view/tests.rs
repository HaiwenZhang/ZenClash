use super::*;
use gpui_kit::AppContext;
use gpui_kit::component::WindowExt;

#[gpui_kit::test]
fn pending_history_controls_do_not_relabel_the_completed_query(cx: &mut gpui_kit::TestAppContext) {
    use crate::pages::Page;
    use crate::pages::runtime::test_support::{
        RuntimePageFixture as Fixture, open_runtime_page_window as open,
    };
    use gpui_kit::test::TestWindowExt;
    let fixture = Fixture::new();
    let services = fixture.services();
    let store = services.traffic_history_store.unwrap();
    let now = super::super::unix_millis();
    store
        .insert_and_cleanup(
            &[zenclash_core::TrafficHistoryEntry {
                timestamp_ms: now,
                host: "host.example".into(),
                process: "browser.exe".into(),
                source_ip: "127.0.0.1".into(),
                outbound: "DIRECT".into(),
                upload: 8 * 1024 * 1024 * 1024,
                download: 16 * 1024 * 1024 * 1024,
            }],
            0,
        )
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Traffic);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle(cx, &page, |page| {
        page.traffic_history.completed_query().is_some()
    });
    let query_gate = cx.update(|cx| page.read(cx).traffic_history.query_gate_for_test());
    let held_query = services.runtime.block_on(query_gate.lock_owned());
    cx.update(|cx| page.update(cx, |page, cx| page.refresh_traffic_history(cx)));
    let summary_for = |range: TrafficRange| {
        zenclash_i18n::text_with(
            "traffic.chart.summary",
            &[
                ("range", range.label()),
                ("upload", format_bytes(8 * 1024 * 1024 * 1024)),
                ("download", format_bytes(16 * 1024 * 1024 * 1024)),
            ],
        )
    };
    cx.update_window(window, |_, window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        window.click(("traffic-dimension", 3usize), cx);
        window.click(("traffic-range", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("traffic-ranking-title").label(),
            Some(
                zenclash_i18n::text_with(
                    "traffic.rankings.title",
                    &[("dimension", dimension_label(TrafficDimension::Host))]
                )
                .as_str()
            )
        );
        assert_eq!(
            window.find("traffic-history-summary").label(),
            Some(summary_for(TrafficRange::Day).as_str())
        );
    })
    .unwrap();
    drop(held_query);
    fixture.settle(cx, &page, |page| !page.traffic_history.is_loading());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("traffic-ranking-title").label(),
            Some(
                zenclash_i18n::text_with(
                    "traffic.rankings.title",
                    &[("dimension", dimension_label(TrafficDimension::Process))]
                )
                .as_str()
            )
        );
        assert_eq!(
            window.find("traffic-history-summary").label(),
            Some(summary_for(TrafficRange::Week).as_str())
        );
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn history_chart_keeps_upload_and_download_as_distinct_series() {
    let points = historical_traffic_points(
        &[TrafficTrendPoint {
            timestamp_ms: 1_700_000_000_000,
            upload: 1_024,
            download: 8_192,
        }],
        TrafficRange::Hour,
    );

    assert_eq!(points.len(), 1);
    assert_eq!(points[0].upload, 1_024.0);
    assert_eq!(points[0].download, 8_192.0);
    assert!(!points[0].label.is_empty());
}

#[test]
fn history_chart_preserves_distinct_large_bucket_magnitudes() {
    let gib = 1024_u64 * 1024 * 1024;
    let points = historical_traffic_points(
        &[
            TrafficTrendPoint {
                timestamp_ms: 1_700_000_000_000,
                upload: 8 * gib,
                download: 16 * gib,
            },
            TrafficTrendPoint {
                timestamp_ms: 1_700_000_300_000,
                upload: 32 * gib,
                download: 64 * gib,
            },
        ],
        TrafficRange::Hour,
    );
    assert_eq!(
        (points[0].upload, points[0].download),
        (8_589_934_592., 17_179_869_184.)
    );
    assert_eq!(
        (points[1].upload, points[1].download),
        (34_359_738_368., 68_719_476_736.)
    );
    assert!(chart_value(u64::MAX).is_finite());
    assert!(chart_value(u64::MAX) > points[1].download);
}
