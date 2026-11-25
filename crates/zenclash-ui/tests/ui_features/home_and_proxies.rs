use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, ElementId, TestAppContext, px};
use zenclash_core::{ProxyCatalog, ProxyGroup, ProxyGroupBehavior, ProxyNode};
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::test_support::controller::ControllerFixture;
use zenclash_ui::test_support::proxies::{
    ProxyConfigurationFixture, has_loaded_catalog, open_catalog, open_ready_session_page_window,
};
use zenclash_ui::test_support::runtime_host::{
    RuntimePageFixture, current_runtime_page, open_runtime_page_window, switch_runtime_page,
};

#[gpui_kit::test]
fn home_dashboard_switches_proxy_group_and_selects_node_via_controller(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    controller.set_proxy_catalog(serde_json::json!({
        "proxies": {
            "HK-01": { "name": "HK-01", "type": "Shadowsocks" },
            "JP-01": { "name": "JP-01", "type": "Trojan" },
            "SG-01": { "name": "SG-01", "type": "VLESS" },
            "Proxy": {
                "name": "Proxy",
                "type": "Selector",
                "now": "HK-01",
                "all": ["HK-01", "JP-01", "SG-01"]
            },
            "Streaming": {
                "name": "Streaming",
                "type": "Selector",
                "now": "SG-01",
                "all": ["HK-01", "JP-01", "SG-01"]
            }
        }
    }));

    let fixture = RuntimePageFixture::with_controller(controller.endpoint_url().to_owned());
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Home);
    fixture.wait_for_initial_reads(cx, &page);
    cx.run_until_parked();

    let group_id =
        |name: &str| ElementId::from((ElementId::from("home-group-card"), name.to_owned()));
    let node_id =
        |name: &str| ElementId::from((ElementId::from("home-node-card"), name.to_owned()));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(group_id("Proxy")).selected(), Some(true));
        assert_eq!(window.find(node_id("HK-01")).selected(), Some(true));

        window.click(group_id("Streaming"), cx);
        window.render_frame(cx);
        assert_eq!(window.find(group_id("Streaming")).selected(), Some(true));
        assert_eq!(window.find(node_id("SG-01")).selected(), Some(true));

        window.click(node_id("JP-01"), cx);
    })
    .unwrap();

    // Wait for loopback selection request to complete and read back.
    fixture.wait_until(cx, |_| !controller.proxy_selections().is_empty());
    fixture.wait_for_initial_reads(cx, &page);

    assert_eq!(
        controller.proxy_selections(),
        vec![(
            "/proxies/Streaming".to_owned(),
            serde_json::json!({ "name": "JP-01" }),
        )]
    );

    switch_runtime_page(cx, &page, Page::Profiles);
    assert_eq!(current_runtime_page(cx, &page), Page::Profiles);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn proxies_page_loads_live_mock_controller_catalog(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });

    let fixture = ProxyConfigurationFixture::new();
    let (window, page) = cx
        .foreground_executor()
        .clone()
        .block_test(open_ready_session_page_window(cx, &fixture));
    assert!(has_loaded_catalog(cx, &page));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("proxy-group-navigation").visible());
        assert!(window.find("proxy-node-panel").visible());
        assert!(window.find("refresh-proxies").visible());
        assert!(window.find("sort-proxies-by-latency").visible());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn proxies_page_switches_groups_and_accepts_search_filter_input(cx: &mut TestAppContext) {
    let nodes_primary = vec![
        ProxyNode {
            name: "Tokyo-01".into(),
            kind: "Shadowsocks".into(),
            ..Default::default()
        },
        ProxyNode {
            name: "Frankfurt-02".into(),
            kind: "Trojan".into(),
            ..Default::default()
        },
    ];
    let nodes_backup = vec![ProxyNode {
        name: "Singapore-03".into(),
        kind: "VLESS".into(),
        ..Default::default()
    }];
    let catalog = ProxyCatalog::from_group_nodes(
        vec![
            (
                ProxyGroup::new("Primary")
                    .with_kind("Selector")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("Tokyo-01"),
                nodes_primary,
            ),
            (
                ProxyGroup::new("Backup")
                    .with_kind("Selector")
                    .with_behavior(ProxyGroupBehavior::Selector)
                    .with_current_member("Singapore-03"),
                nodes_backup,
            ),
        ],
        3,
    );

    let (window, _page, _runtime) = open_catalog(cx, catalog);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("proxy-group-list").visible());
        assert!(window.find("proxy-node-viewport").visible());

        window.click("proxy-node-search", cx);
        window.input("Tokyo", cx);
        let search = window.find("proxy-node-search");
        assert_eq!(search.value(), Some("Tokyo"));
        assert!(search.bounds().size.width > px(0.));

        window.click("sort-proxies-by-latency", cx);
        window.render_frame(cx);
        assert!(window.find("proxy-node-panel").visible());

        window.remove_window();
    })
    .unwrap();
}
