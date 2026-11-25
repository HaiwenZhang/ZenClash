use gpui_kit::component::{ActiveTheme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_app::SafeMode;
use zenclash_ui::foundation::actions::Page;
use zenclash_ui::foundation::design::apply_zen_theme;
use zenclash_ui::shell::app::reopen_main_window;
use zenclash_ui::test_support::runtime_host::RuntimePageFixture;
use zenclash_ui::test_support::shell_tray::{
    TrayController, app_current_page, app_sidebar_collapsed, open_app, open_app_with_safe_mode,
};

#[gpui_kit::test]
fn zenclash_app_navigates_pages_toggles_sidebar_and_switches_theme(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let controller = TrayController::new(&fixture.runtime_handle(), None);
    let (window, app) = open_app(cx, &fixture, controller.session());

    assert_eq!(app_current_page(cx, &app), Page::Home);
    assert!(!app_sidebar_collapsed(cx, &app));

    cx.update_window(window, |_, window, cx| {
        apply_zen_theme(ThemeMode::Light, Some(window), cx);
        window.render_frame(cx);
        assert!(window.try_find("safe-mode-banner").is_none());
        window.click(Page::Proxies.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(app_current_page(cx, &app), Page::Proxies);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(Page::Profiles.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(app_current_page(cx, &app), Page::Profiles);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(Page::Settings.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(app_current_page(cx, &app), Page::Settings);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-sidebar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(app_sidebar_collapsed(cx, &app));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-sidebar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!app_sidebar_collapsed(cx, &app));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Light);
        window.click("sidebar-appearance", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        window.click("sidebar-appearance", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Light);
        window.click(Page::Home.route(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(app_current_page(cx, &app), Page::Home);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn zenclash_app_displays_safe_mode_banner_and_handles_reopen(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let controller = TrayController::new(&fixture.runtime_handle(), None);
    let (window, app) =
        open_app_with_safe_mode(cx, &fixture, controller.session(), SafeMode::enabled());

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let banner = window.find("safe-mode-banner");
        assert!(banner.visible());
        assert_eq!(banner.label(), Some("SAFE MODE"));
    })
    .unwrap();

    cx.update(reopen_main_window);
    cx.run_until_parked();
    assert_eq!(app_current_page(cx, &app), Page::Home);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
