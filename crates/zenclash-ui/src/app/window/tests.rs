use super::*;
use crate::app::test_support::app_test_services;
#[cfg(target_os = "windows")]
use crate::components::tray::TrayCommand;
use gpui_kit::{px, size};

#[gpui_kit::test]
fn application_reopen_restores_hidden_main_window(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::Root;
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app =
            cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();
    // The headless platform has no native application-hide implementation.
    // Reproduce the retained window state established by the close handler.
    cx.update_window(main.into(), |_, _window, cx| {
        owner.update(cx, |app, cx| {
            #[cfg(target_os = "macos")]
            app.park_main_window(_window);
            app.release_hidden_page_data(cx);
        });
    })
    .unwrap();
    assert!(!cx.update(|cx| owner.read(cx).main_window_visible));
    cx.update(crate::app::bootstrap::reopen_main_window);
    assert!(cx.update(|cx| owner.read(cx).main_window_visible));
    #[cfg(target_os = "macos")]
    assert!(cx.update(|cx| {
        owner
            .read(cx)
            .main_window_restore_size
            .restore_size
            .is_none()
    }));
    cx.update(crate::app::bootstrap::reopen_main_window);
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    cx.update_window(main.into(), |_, window, _| window.remove_window())
        .unwrap();
}

#[cfg(target_os = "windows")]
#[gpui_kit::test]
fn caption_close_keeps_app_alive_and_tray_show_restores_the_same_window(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app =
            cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();

    cx.update_window(main.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("main-window-close", cx);
        assert!(!owner.read(cx).main_window_visible);
        assert_eq!(
            owner.read(cx).quit_state,
            crate::app::system_proxy::QuitState::Idle
        );
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(cx.windows().len(), 1);
        owner.update(cx, |app, cx| {
            app.handle_tray_command(TrayCommand::ShowWindow, cx)
        });
        assert!(owner.read(cx).main_window_visible);
        assert_eq!(owner.read(cx).main_window, main.into());
    });
    cx.update_window(main.into(), |_, window, _| window.remove_window())
        .unwrap();
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[gpui_kit::test]
fn native_close_keeps_app_alive_and_reopen_restores_the_same_window(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app =
            cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();
    // The headless platform has no native close-event dispatcher. Exercise
    // the exact production callback, including its veto of window removal.
    cx.update_window(main.into(), |_, window, cx| {
        assert!(!crate::app::bootstrap::retain_main_window_on_close(
            window,
            &owner.downgrade(),
            cx
        ));
        assert!(!owner.read(cx).main_window_visible);
        assert_eq!(
            owner.read(cx).quit_state,
            crate::app::system_proxy::QuitState::Idle
        );
    })
    .unwrap();
    cx.update(crate::app::bootstrap::reopen_main_window);
    cx.update(|cx| {
        assert_eq!(cx.windows().len(), 1);
        assert!(owner.read(cx).main_window_visible);
        assert_eq!(owner.read(cx).main_window, main.into());
    });
    cx.update_window(main.into(), |_, window, _| window.remove_window())
        .unwrap();
}

#[cfg(target_os = "linux")]
#[gpui_kit::test]
fn taskbar_activation_restores_hidden_main_window_data(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::Root;
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app =
            cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    let owner = owner.unwrap();
    cx.update_window(main.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(cx.update(|cx| owner.read(cx).runtime_page.read(cx).live_updates_enabled()));
    cx.update_window(main.into(), |_, window, cx| {
        owner.update(cx, |app, cx| app.hide_main_window_in(window, cx));
    })
    .unwrap();
    assert!(!cx.update(|cx| owner.read(cx).main_window_visible));
    assert!(!cx.update(|cx| owner.read(cx).runtime_page.read(cx).live_updates_enabled()));
    gpui_kit::VisualTestContext::from_window(main.into(), cx).deactivate_window();

    // Native activation models taskbar/compositor restore, without a tray command.
    cx.update_window(main.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(owner.read(cx).main_window_visible);
        assert!(owner.read(cx).runtime_page.read(cx).live_updates_enabled());
        assert_eq!(owner.read(cx).main_window, main.into());
        assert_eq!(
            owner.read(cx).quit_state,
            crate::app::system_proxy::QuitState::Idle
        );
    });
    cx.update_window(main.into(), |_, window, _| window.remove_window())
        .unwrap();
}

#[cfg(all(test, target_os = "macos"))]
mod restore_size_tests {
    use super::super::MainWindowRestoreSize;
    use gpui_kit::px;

    #[test]
    fn parked_main_window_restores_its_previous_size() {
        let mut state = MainWindowRestoreSize::default();
        let visible_size = gpui_kit::size(px(1280.), px(820.));

        assert_eq!(state.park(visible_size), gpui_kit::size(px(1.), px(1.)));
        assert_eq!(state.restore(), Some(visible_size));
        assert_eq!(state.restore(), None);
    }

    #[test]
    fn repeated_parking_does_not_forget_the_visible_size() {
        let mut state = MainWindowRestoreSize::default();
        let visible_size = gpui_kit::size(px(960.), px(640.));

        state.park(visible_size);
        state.park(gpui_kit::size(px(1.), px(1.)));

        assert_eq!(state.restore(), Some(visible_size));
    }
}
