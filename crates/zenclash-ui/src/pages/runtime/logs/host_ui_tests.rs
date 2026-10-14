use crate::pages::runtime::test_support::{
    ControllerFixture, RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::fs;

#[gpui_kit::test]
fn log_collection_menu_updates_controlled_configuration_and_monitor(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let preferences =
        zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"));
    let original = fs::read_to_string(&fixture.profile).unwrap();
    let (window, page) = open(cx, &fixture, Page::Logs);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.preferences_store = Some(preferences.clone());
            cx.notify();
        });
        window.render_frame(cx);
        window.click("log-settings", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("log-collection-level", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("save-log-settings", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy()
            && page.log_monitor.level() == MihomoLogLevel::Warning
            && page.config_inputs.effective()["log-level"].as_str() == Some("warning")
    });
    assert_eq!(fs::read_to_string(&fixture.profile).unwrap(), original);
    assert_eq!(fixture.logs.level(), MihomoLogLevel::Warning);
    assert_eq!(
        controller
            .apply_requests
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
