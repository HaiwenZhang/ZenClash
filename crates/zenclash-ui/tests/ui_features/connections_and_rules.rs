use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::{Connection, ConnectionsSnapshot, Rule, RuleCatalog};
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::test_support::controller::ControllerFixture;
use zenclash_ui::test_support::runtime_host::{
    RuntimePageFixture, open_runtime_page_window, seed_connections_snapshot,
};

#[gpui_kit::test]
fn connections_page_renders_seeded_snapshots_and_toggles_closed_and_pause(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Connections);
    fixture.wait_for_initial_reads(cx, &page);

    seed_connections_snapshot(
        cx,
        &page,
        ConnectionsSnapshot {
            connections: vec![
                Connection {
                    id: "conn-closed".into(),
                    ..Default::default()
                },
                Connection {
                    id: "conn-live".into(),
                    ..Default::default()
                },
            ],
            upload_total: 1024,
            download_total: 2048,
            memory: 4096,
        },
    );
    fixture.settle_connections(cx, &page);

    seed_connections_snapshot(
        cx,
        &page,
        ConnectionsSnapshot {
            connections: vec![Connection {
                id: "conn-live".into(),
                ..Default::default()
            }],
            upload_total: 2048,
            download_total: 4096,
            memory: 4096,
        },
    );
    fixture.settle_connections(cx, &page);

    cx.update_window(window, |_, window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        assert!(window.find("connections-closed").visible());
        assert!(window.find("pause-connections-display").visible());

        window.click("connections-closed", cx);
    })
    .unwrap();
    fixture.settle_connections(cx, &page);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("pause-connections-display", cx);
        window.render_frame(cx);
        window.click("pause-connections-display", cx);
        window.render_frame(cx);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn rules_page_renders_mock_controller_rules_and_opens_rule_inspector(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    controller.set_rule_catalog(RuleCatalog {
        rules: vec![
            Rule {
                index: Some(0),
                kind: "DOMAIN-SUFFIX".into(),
                payload: "example.com".into(),
                proxy: "Proxy".into(),
                ..Default::default()
            },
            Rule {
                index: Some(1),
                kind: "MATCH".into(),
                payload: String::new(),
                proxy: "DIRECT".into(),
                ..Default::default()
            },
        ],
    });

    let fixture = RuntimePageFixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Rules);
    fixture.wait_for_initial_reads(cx, &page);
    fixture.settle_rules(cx, &page);
    cx.run_until_parked();

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("rule-row", 0usize)).visible());
        assert!(window.find(("rule-details", 0usize)).visible());

        window.click(("rule-details", 0usize), cx);
        window.render_frame(cx);
        assert!(window.find("copy-rule").visible());
        assert!(window.find("close-rule-details").visible());

        window.click("close-rule-details", cx);
        window.render_frame(cx);
        window.remove_window();
    })
    .unwrap();
}
