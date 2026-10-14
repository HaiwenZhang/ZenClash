use super::super::test_support::{TrayController, open_app};
use super::*;
use gpui_kit::{AppContext, TestAppContext};

fn snapshot(runtime: TrayRuntimeObservation) -> TrayMenuSnapshot {
    TrayMenuSnapshot {
        current_node: current_proxy_node(
            &runtime.catalog,
            OutboundMode::from_api(&runtime.config.mode),
        ),
        groups: tray_proxy_groups(runtime.catalog, &runtime.config.mode),
        config: runtime.config,
        observation: runtime.identity,
        profile_path: runtime.profile_path,
        system_proxy: Ok(false),
        profiles: Err("isolated fixture".into()),
        directories: Vec::new(),
        mode_intent_generation: 0,
    }
}

#[gpui_kit::test]
fn a_late_tray_snapshot_cannot_replace_new_configuration_menu_facts(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let old = services
        .runtime
        .block_on(TrayRuntimeObservation::load(&session))
        .unwrap();
    let (window, app) = open_app(cx, &fixture, session.clone());
    services
        .runtime
        .block_on(session.set_mode(&controller.store, "global"))
        .unwrap();
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.tray_state.mode = "current menu".into();
            app.tray_current_node = Some((OutboundMode::Global, "current node".into()));
            app.tray_error = Some("current error".into());
            app.apply_tray_menu_state(snapshot(old), cx);
            assert_eq!(app.tray_state.mode, "current menu");
            assert_eq!(app.tray_current_node.as_ref().unwrap().1, "current node");
            assert_eq!(app.tray_error.as_deref(), Some("current error"));
            assert!(app.tray_refresh_pending);
        })
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn a_tray_snapshot_from_another_actual_session_cannot_publish_equal_counter_facts(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let foreign =
        zenclash_core::CoreSession::open(zenclash_core::CoreKind::Mihomo, session.client().clone())
            .unwrap();
    assert_eq!(session.generation(), foreign.generation());
    let observed = services
        .runtime
        .block_on(TrayRuntimeObservation::load(&foreign))
        .unwrap();
    let (window, app) = open_app(cx, &fixture, session);
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.tray_state.mode = "original".into();
            app.apply_tray_menu_state(snapshot(observed), cx);
            assert_eq!(app.tray_state.mode, "original");
            assert!(app.tray_observation.is_none());
        })
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn current_tray_facts_and_source_are_published_together_before_native_menu_work(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let observed = services
        .runtime
        .block_on(TrayRuntimeObservation::load(&session))
        .unwrap();
    let identity = observed.identity.clone();
    let (window, app) = open_app(cx, &fixture, session);
    cx.update(|cx| {
        app.update(cx, |app, cx| {
            app.apply_tray_menu_state(snapshot(observed), cx);
            assert_eq!(app.tray_state.mode, "rule");
            assert_eq!(app.tray_state.groups[0].controller_name(), "group");
            assert!(
                app.tray_observation
                    .as_ref()
                    .unwrap()
                    .is_same_observation(&identity)
            );
            assert_eq!(app.tray_current_node.as_ref().unwrap().1, "node");
            assert!(app.tray_error.is_none());
        })
    });
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn tray_refresh_http_failures_keep_their_original_configuration_and_pending_query(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    controller.fail_catalog_reads();
    let session = controller.session();
    let source = RuntimeReadSource::default();
    let error = services
        .runtime
        .block_on(TrayRuntimeObservation::load_with_source(&session, &source))
        .err()
        .unwrap();
    assert!(error.contains("503"));
    assert!(source.identity().is_some());
    let (window, app) = open_app(cx, &fixture, session.clone());
    cx.update(|cx| {
        app.update(cx, |app, _| {
            assert!(app.tray_observation.is_none());
            assert!(app.publish_tray_refresh_error(&source, error.clone()));
            assert!(app.tray_error.as_deref().unwrap().contains("503"));
        })
    });
    services
        .runtime
        .block_on(session.set_mode(&controller.store, "global"))
        .unwrap();
    cx.update(|cx| {
        app.update(cx, |app, _| {
            app.tray_error = Some("current configuration feedback".into());
            assert!(!app.publish_tray_refresh_error(&source, error));
            assert_eq!(
                app.tray_error.as_deref(),
                Some("current configuration feedback")
            );
            assert!(app.tray_refresh_pending);
        })
    });
    let current_source = RuntimeReadSource::default();
    let current_error = services
        .runtime
        .block_on(TrayRuntimeObservation::load_with_source(
            &session,
            &current_source,
        ))
        .err()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, _| {
            app.tray_refresh_pending = false;
            assert!(app.publish_tray_refresh_error(&current_source, current_error.clone()));
            assert!(app.tray_error.as_deref().unwrap().contains("503"));
            app.tray_refresh_pending = true;
            app.tray_error = Some("pending newer refresh".into());
            assert!(!app.publish_tray_refresh_error(&current_source, current_error));
            assert_eq!(app.tray_error.as_deref(), Some("pending newer refresh"));
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_panicked_tray_refresh_keeps_its_source_for_worker_error_publication(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    let session = controller.session();
    let (window, app) = open_app(cx, &fixture, session.clone());
    let source = std::sync::Arc::new(RuntimeReadSource::default());
    let submitted = source.clone();
    let reading = session.clone();
    let error = services
        .runtime
        .block_on(services.runtime.spawn(async move {
            TrayRuntimeObservation::load_with_source(&reading, &submitted)
                .await
                .unwrap();
            panic!("fixture menu worker failed after its controller read");
        }))
        .unwrap_err()
        .to_string();
    cx.update(|cx| {
        app.update(cx, |app, _| {
            assert!(app.publish_tray_refresh_error(&source, error.clone()));
            assert!(
                app.tray_error
                    .as_deref()
                    .unwrap()
                    .contains("fixture menu worker")
            );
        })
    });
    services
        .runtime
        .block_on(session.set_mode(&controller.store, "global"))
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, _| {
            app.tray_error = Some("current configuration feedback".into());
            assert!(!app.publish_tray_refresh_error(&source, error));
            assert_eq!(
                app.tray_error.as_deref(),
                Some("current configuration feedback")
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_foreign_tray_refresh_failure_cannot_publish_equal_counter_diagnostics(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = fixture.services();
    let controller = TrayController::new(&services.runtime, None);
    controller.fail_catalog_reads();
    let session = controller.session();
    let foreign =
        zenclash_core::CoreSession::open(zenclash_core::CoreKind::Mihomo, session.client().clone())
            .unwrap();
    assert_eq!(session.generation(), foreign.generation());
    let source = RuntimeReadSource::default();
    let error = services
        .runtime
        .block_on(TrayRuntimeObservation::load_with_source(&foreign, &source))
        .err()
        .unwrap();
    let (window, app) = open_app(cx, &fixture, session);
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, _| {
            app.tray_error = Some("actual owner feedback".into());
            assert!(!app.publish_tray_refresh_error(&source, error));
            assert_eq!(app.tray_error.as_deref(), Some("actual owner feedback"));
        });
        window.remove_window();
    })
    .unwrap();
}
