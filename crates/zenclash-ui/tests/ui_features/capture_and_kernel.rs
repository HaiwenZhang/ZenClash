use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::RuntimeConfig;
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::test_support::runtime_host::{
    RuntimePageFixture, current_runtime_page, open_runtime_page_window, seed_runtime_config,
    switch_runtime_page,
};

#[gpui_kit::test]
fn capture_pages_render_system_proxy_editor_and_tun_controls(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::SystemProxy);
    fixture.wait_for_initial_reads(cx, &page);
    seed_runtime_config(cx, &page, RuntimeConfig::default());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("edit-system-proxy").visible());
        assert!(window.find("edit-proxy-bypass").visible());

        window.click("edit-system-proxy", cx);
        window.render_frame(cx);
        assert!(window.find("save-system-proxy").visible());
        assert!(window.find("cancel-system-proxy").visible());

        window.click("cancel-system-proxy", cx);
        window.render_frame(cx);
        assert!(window.find("edit-system-proxy").visible());
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Tun);
    fixture.wait_for_initial_reads(cx, &page);
    seed_runtime_config(cx, &page, RuntimeConfig::default());
    assert_eq!(current_runtime_page(cx, &page), Page::Tun);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("tun-control-status").visible());
        assert!(window.find("save-tun-advanced").visible());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn kernel_pages_render_mihomo_dns_and_sniffer_config_controls(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Mihomo);
    fixture.wait_for_initial_reads(cx, &page);
    seed_runtime_config(cx, &page, RuntimeConfig::default());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("open-core-updates").visible());
        assert!(window.find("auto-core-binary").visible());
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Dns);
    fixture.wait_for_initial_reads(cx, &page);
    seed_runtime_config(cx, &page, RuntimeConfig::default());
    assert_eq!(current_runtime_page(cx, &page), Page::Dns);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("save-dns-advanced").visible());
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Sniffer);
    fixture.wait_for_initial_reads(cx, &page);
    seed_runtime_config(cx, &page, RuntimeConfig::default());
    assert_eq!(current_runtime_page(cx, &page), Page::Sniffer);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("save-sniffer-advanced").visible());
        window.remove_window();
    })
    .unwrap();
}
