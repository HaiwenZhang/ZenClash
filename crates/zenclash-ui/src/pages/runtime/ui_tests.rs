//! Runtime host integration across capabilities; setup lives in test_support.
use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::fs;

#[cfg(target_os = "windows")]
mod design_validation;
mod service_maintenance;
mod service_tun;
mod tray_receipts;

#[test]
fn core_dependency_models_ordinary_gui_privileges_for_ui_tests() {
    // Core is a dependency here, so its own cfg(test) cannot isolate CI privileges.
    assert!(!zenclash_core::current_process_elevated());
}

#[gpui_kit::test]
fn offline_pages_render_local_content_and_delete_a_disabled_override(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let source = fixture.root.join("local-override.yaml");
    fs::write(&source, "allow-lan: true\n").unwrap();
    let record = fixture.overrides.import_paths([source]).unwrap().remove(0);
    fixture.overrides.set_enabled(&record.id, false).unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Empty);
            cx.notify();
        });
        window.render_frame(cx);
        let profile = fixture.profiles.load().unwrap().profiles.remove(0);
        assert_eq!(
            window.find(format!("profile-more:{}", profile.id)).label(),
            Some(
                zenclash_i18n::text_with("profiles.actions.more", &[("name", profile.name)])
                    .as_str()
            )
        );
        page.update(cx, |page, cx| page.switch_to(Page::Override, cx));
        window.render_frame(cx);
        window.find("preview-overrides");
        window.click(format!("override-delete:{}", record.id), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.catalog().items.is_empty());
    assert!(fixture.overrides.load().unwrap().items.is_empty());
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.switch_to(Page::Traffic, cx);
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Empty);
            cx.notify();
        });
        window.render_frame(cx);
        window.find("request-clear-traffic");
        window.remove_window();
    })
    .unwrap();
}
