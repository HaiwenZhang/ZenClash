use gpui_kit::AppContext;
use gpui_kit::test::TestWindowExt;

#[gpui_kit::test]
fn ranking_and_detail_rows_are_keyboard_focusable_and_activatable(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::foundation::actions::Page;
    use crate::test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open};

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
                upload: 2048,
                download: 4096,
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

    let ranking_id = (
        gpui_kit::ElementId::from("traffic-ranking"),
        "host.example".to_owned(),
    );
    cx.update_window(window, |_, window, cx| {
        use gpui_kit::Focusable;
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        assert_eq!(
            window.find(ranking_id.clone()).label(),
            Some("host.example")
        );
        for _ in 0..40 {
            if window.find(ranking_id.clone()).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(
            window.find(ranking_id.clone()).focused(),
            Some(true),
            "traffic ranking row must be reachable by Tab"
        );
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.traffic_history.is_loading()
            && page
                .traffic_history
                .presentation()
                .and_then(|presentation| presentation.selected_parent())
                == Some("host.example")
    });

    let detail_label = cx.update(|cx| {
        page.read(cx)
            .traffic_history
            .presentation()
            .unwrap()
            .details()[0]
            .label
            .clone()
    });
    let detail_id = (
        gpui_kit::ElementId::from("traffic-detail"),
        detail_label.clone(),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(detail_id.clone()).label(),
            Some(detail_label.as_str())
        );
        for _ in 0..40 {
            if window.find(detail_id.clone()).focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(
            window.find(detail_id.clone()).focused(),
            Some(true),
            "traffic detail row must be reachable by Tab"
        );
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.traffic_history.is_loading()
            && page
                .traffic_history
                .presentation()
                .and_then(|presentation| presentation.selected_detail())
                == Some(detail_label.as_str())
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
