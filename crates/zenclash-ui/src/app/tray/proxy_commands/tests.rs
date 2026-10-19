use super::super::test_support::{TrayController, open_app};
use crate::components::tray::TrayCommand;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use std::time::Duration;

#[gpui_kit::test]
fn queued_tray_selection_keeps_its_original_source_after_runtime_replacement(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
        let services = fixture.services();
        let mut original = TrayController::new(&services.runtime, Some("PUT /proxies/group "));
        let replacement = TrayController::new(&services.runtime, None);
        let session = original.session();
        let source = services
            .runtime
            .block_on(session.begin_observation())
            .unwrap()
            .identity()
            .clone();
        let (window, app) = open_app(cx, &fixture, session.clone());
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.tray_observation = Some(source.clone());
                app.handle_observed_tray_command(
                    TrayCommand::SelectProxy {
                        group: "group".into(),
                        proxy: "Requested".into(),
                    },
                    Some(source.clone()),
                    cx,
                );
            })
        });
        let started = original.started.take().unwrap();
        services
            .runtime
            .spawn(async move {
                tokio::time::timeout(Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
            })
            .await
            .unwrap();
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.handle_observed_tray_command(
                    TrayCommand::SelectProxy {
                        group: "group".into(),
                        proxy: "Second".into(),
                    },
                    Some(source.clone()),
                    cx,
                );
                assert!(app.proxy_selection_commands.is_running());
            })
        });
        original.release.take().unwrap().send(()).unwrap();
        services
            .runtime
            .block_on(session.switch_to_direct(zenclash_core::MihomoEndpoint::new(
                &replacement.endpoint,
                "",
            )))
            .unwrap();
        let current = services
            .runtime
            .block_on(session.begin_observation())
            .unwrap()
            .identity()
            .clone();
        cx.update(|cx| {
            app.update(cx, |app, _| {
                app.tray_observation = Some(current);
                app.tray_command_error = Some("new menu feedback".into());
            })
        });
        for _ in 0..200 {
            if cx.update(|cx| !app.read(cx).proxy_selection_commands.is_running()) {
                break;
            }
            services
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        cx.update_window(window, |_, window, cx| {
            assert!(!app.read(cx).proxy_selection_commands.is_running());
            assert_eq!(
                app.read(cx).tray_command_error.as_deref(),
                Some("new menu feedback")
            );
            window.remove_window();
        })
        .unwrap();
        assert_eq!(
            original
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.starts_with("PUT /proxies/group "))
                .count(),
            1
        );
        assert!(
            !replacement
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("PUT /proxies/"))
        );
    });
}

#[gpui_kit::test]
fn old_tray_feedback_cannot_replace_a_new_menu_query_in_the_same_configuration(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let old = services
        .runtime
        .block_on(session.begin_observation())
        .unwrap()
        .identity()
        .clone();
    let current = services
        .runtime
        .block_on(session.begin_observation())
        .unwrap()
        .identity()
        .clone();
    let (window, app) = open_app(cx, &fixture, session);
    cx.update(|cx| {
        app.update(cx, |app, _| {
            app.tray_observation = Some(current);
            app.tray_command_error = Some("current query".into());
            assert!(
                !app.publish_tray_proxy_feedback(&old, |app| app.tray_command_error =
                    Some("old operation failed".into()))
            );
            assert_eq!(app.tray_command_error.as_deref(), Some("current query"));
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tray-feedback").label(), Some("current query"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_proxy_command_from_an_unobserved_native_menu_is_rejected(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let (window, app) = open_app(cx, &fixture, session);
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.handle_observed_tray_command(
                TrayCommand::SelectProxy {
                    group: "group".into(),
                    proxy: "Requested".into(),
                },
                None,
                cx,
            );
            assert!(!app.proxy_selection_commands.is_running());
            assert!(app.tray_command_error.is_some());
        })
    });
    assert!(
        !controller
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.starts_with("PUT /proxies/"))
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("tray-feedback").visible());
        assert_eq!(
            window.find("tray-feedback").label(),
            app.read(cx).tray_command_error.as_deref()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn current_tray_selection_is_admitted_and_confirms_on_its_original_controller(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
        let services = fixture.services();
        let controller = TrayController::new(&services.runtime, None);
        let session = controller.session();
        let source = services
            .runtime
            .block_on(session.begin_observation())
            .unwrap()
            .identity()
            .clone();
        let (window, app) = open_app(cx, &fixture, session);
        cx.update(|cx| {
            app.update(cx, |app, cx| {
                app.tray_observation = Some(source.clone());
                app.handle_observed_tray_command(
                    TrayCommand::SelectProxy {
                        group: "group".into(),
                        proxy: "Requested".into(),
                    },
                    Some(source),
                    cx,
                );
            })
        });
        for _ in 0..200 {
            if cx.update(|cx| !app.read(cx).proxy_selection_commands.is_running()) {
                break;
            }
            services
                .runtime
                .spawn(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        assert!(
            controller
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("PUT /proxies/group "))
        );
        assert!(
            controller
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("GET /proxies/group "))
        );
        cx.update_window(window, |_, window, cx| {
            assert!(!app.read(cx).proxy_selection_commands.is_running());
            assert!(app.read(cx).tray_command_error.is_none());
            window.remove_window();
        })
        .unwrap();
    });
}
