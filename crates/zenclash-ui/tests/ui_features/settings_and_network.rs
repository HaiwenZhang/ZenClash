use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::AppPreferencesStore;
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::test_support::resources::ResourceActionController;
use zenclash_ui::test_support::runtime_host::{
    RuntimePageFixture, current_runtime_page, open_runtime_page_window, seed_preferences_store,
    switch_runtime_page,
};

#[gpui_kit::test]
fn settings_page_opens_license_dialog_and_persists_traffic_history_preference(
    cx: &mut TestAppContext,
) {
    let fixture = RuntimePageFixture::new();
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Settings);
    fixture.wait_for_initial_reads(cx, &page);

    let store = AppPreferencesStore::new(fixture.root_dir().join("preferences.json"));
    let initial = store
        .update(|preferences| preferences.traffic_history_enabled = false)
        .unwrap();
    seed_preferences_store(cx, &page, store.clone(), initial);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-language").visible());
        assert!(window.find("theme-light").visible());
        assert!(window.find("theme-dark").visible());
        assert!(window.find("theme-system").visible());
        assert!(window.find("settings-traffic-history").visible());

        window.click("settings-license-notices", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.find("license-notices-scroll").visible());

        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));

        window.click("settings-traffic-history", cx);
    })
    .unwrap();

    fixture.wait_until(cx, |_| store.load().unwrap().traffic_history_enabled);
    assert!(store.load().unwrap().traffic_history_enabled);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn network_traffic_logs_and_resources_pages_render_interactive_controls(cx: &mut TestAppContext) {
    let resources_controller = ResourceActionController::new("Airport");
    let fixture =
        RuntimePageFixture::with_controller(resources_controller.endpoint_url().to_owned());
    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Network);
    fixture.wait_for_initial_reads(cx, &page);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("refresh-network-probe").visible());
        assert!(window.find("network-options").visible());

        window.click("network-options", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.find("dialog-refresh-network-probe").visible());

        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Traffic);
    fixture.wait_for_initial_reads(cx, &page);
    assert_eq!(current_runtime_page(cx, &page), Page::Traffic);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("traffic-history-summary").visible());
        assert!(window.find("request-clear-traffic").visible());
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Logs);
    fixture.wait_for_initial_reads(cx, &page);
    assert_eq!(current_runtime_page(cx, &page), Page::Logs);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("pause-logs-display").visible());
        assert!(window.find("log-settings").visible());
        assert!(window.find("clear-logs").visible());

        window.click("log-settings", cx);
        window.render_frame(cx);
        assert!(window.find("cancel-log-settings").visible());
        window.click("cancel-log-settings", cx);
        window.render_frame(cx);
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Resources);
    fixture.wait_for_initial_reads(cx, &page);
    assert_eq!(current_runtime_page(cx, &page), Page::Resources);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("update-geodata").visible());
        assert!(window.find("open-ruleset-converter").visible());
        window.remove_window();
    })
    .unwrap();
}
