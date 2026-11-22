use std::fs;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::test_support::runtime_host::{
    RuntimePageFixture, current_runtime_page, is_core_busy, open_runtime_page_window,
    switch_runtime_page,
};

#[gpui_kit::test]
fn profiles_page_renders_local_catalog_and_navigates_to_override_page(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let extra_profile = fixture.root_dir().join("secondary.yaml");
    fs::write(&extra_profile, "mixed-port: 7895\nrules: [MATCH,DIRECT]\n").unwrap();
    fixture.profiles().import_local(extra_profile).unwrap();

    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Profiles);
    fixture.wait_for_initial_reads(cx, &page);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("profiles-catalog-region").visible());
        assert!(window.find("update-all-profiles").visible());
        assert!(window.find("toggle-add-subscription").visible());
        assert!(window.find("choose-profile").visible());
    })
    .unwrap();

    switch_runtime_page(cx, &page, Page::Override);
    fixture.wait_for_initial_reads(cx, &page);
    assert_eq!(current_runtime_page(cx, &page), Page::Override);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("choose-overrides").visible());
        assert!(window.find("preview-overrides").visible());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn override_page_previews_chain_and_deletes_disabled_override(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let alpha_path = fixture.root_dir().join("alpha.yaml");
    let beta_path = fixture.root_dir().join("beta.yaml");
    fs::write(&alpha_path, "allow-lan: true\n").unwrap();
    fs::write(&beta_path, "ipv6: true\n").unwrap();

    let alpha = fixture
        .overrides()
        .import_paths([alpha_path])
        .unwrap()
        .remove(0);
    let beta = fixture
        .overrides()
        .import_paths([beta_path])
        .unwrap()
        .remove(0);
    fixture.overrides().set_enabled(&beta.id, false).unwrap();

    let (window, page) = open_runtime_page_window(cx, &fixture, Page::Override);
    fixture.wait_for_initial_reads(cx, &page);
    fixture.settle_override_count(cx, &page, 2);

    let delete_beta_id = format!("override-delete:{}", beta.id);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("preview-overrides").visible());
        assert!(window.find(delete_beta_id.clone()).visible());

        window.click("preview-overrides", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!is_core_busy(cx, &page));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(delete_beta_id, cx);
    })
    .unwrap();

    fixture.settle_override_count(cx, &page, 1);
    let remaining = fixture.overrides().load().unwrap();
    assert_eq!(remaining.items.len(), 1);
    assert_eq!(remaining.items[0].id, alpha.id);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
