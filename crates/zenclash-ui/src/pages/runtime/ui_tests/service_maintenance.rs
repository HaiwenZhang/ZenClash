use crate::pages::runtime::test_support::{
    ControllerFixture, RuntimePageFixture as Fixture, open_service_tun, owned_ui_children,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::fs;

#[gpui_kit::test]
fn service_maintenance_dialogs_cancel_with_keyboard_and_restore_focus(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;

    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let pid = local.snapshot().pid;
    let generation = fixture.core.generation();
    let bytes = fs::read(&local.launch_config().config_file).unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        for id in ["repair-service", "uninstall-service"] {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            for _ in 0..12 {
                if window.find(id).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(id).focused(), Some(true));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(window.find(id).focused(), Some(true));
            window.click(id, cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
        }
        assert_eq!(local.snapshot().pid, pid);
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(fs::read(&local.launch_config().config_file).unwrap(), bytes);
        assert!(
            page.read(cx)
                .application_commands
                .service_maintenance_preparation()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn offline_service_maintenance_dialog_can_be_cancelled_and_missing_core_never_restarts(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut fixture = Fixture::new();
    fixture.status.stop();
    fixture.core = CoreSession::open_offline(
        CoreKind::Mihomo,
        fixture.root.join("offline-home"),
        Some(fixture.root.join("missing-selected-core")),
    )
    .unwrap();
    fixture.status = OperationalStatus::start(
        fixture.runtime.as_ref().unwrap().handle(),
        fixture.core.clone(),
        None,
        fixture.traffic.clone(),
        fixture.logs.clone(),
    );
    let (window, page) = open_service_tun(cx, &fixture);
    let restarts = Arc::new(AtomicUsize::new(0));
    let counter = restarts.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, _: &ServiceRepairRestartRequested, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    });
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let config_before = fs::read(&fixture.profile).unwrap();
    cx.update_window(window, |_, window, cx| {
        for id in ["repair-service", "uninstall-service"] {
            window.render_frame(cx);
            window.focus(&page.read(cx).focus_handle.clone(), cx);
            for _ in 0..12 {
                if window.find(id).focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find(id).focused(), Some(true));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(window.find(id).focused(), Some(true));
        }
        assert!(
            page.read(cx)
                .application_commands
                .service_maintenance_preparation()
                .is_none()
        );
        window.render_frame(cx);
        window.click("repair-service", cx);
        assert!(window.has_active_dialog(cx));
        window.click("ok", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert!(fixture.core.is_offline_recovery());
        assert!(!fixture.core.is_managed());
        assert_eq!(fixture.core.generation(), 0);
        assert_eq!(restarts.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(&fixture.profile).unwrap(), config_before);
        assert_eq!(
            page.read(cx)
                .application_commands
                .service_state()
                .unwrap()
                .phase(),
            zenclash_core::ServicePhase::Failed
        );
        assert!(
            page.read(cx)
                .application_commands
                .service_maintenance_preparation()
                .is_some()
        );
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_maintenance_confirmation_rejects_replaced_owner_before_native_work(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;

    // Keep controller readback successful so it cannot race with the stale
    // maintenance result that this test is asserting.
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading() && !page.loading
    });
    let preferences =
        zenclash_core::AppPreferencesStore::new(fixture.root.join("maintenance-preferences.json"));
    preferences
        .save(&zenclash_core::AppPreferences::default())
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            // Exercise the offline service card while keeping later controller
            // readback available during maintenance completion.
            page.invalidate_page_load();
            page.page_observation
                .replace_fixture_data(PageSnapshotData::Empty);
            page.preferences_store = Some(preferences.clone());
            page.preferences.system_proxy_enabled = true;
            page.preferences.language = zenclash_core::LanguagePreference::En;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("uninstall-service", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = replacement.snapshot().pid;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).error.as_deref(),
            Some(zenclash_i18n::text("core_page.service.stale").as_str())
        );
        assert!(!page.read(cx).preferences.system_proxy_enabled);
        assert_eq!(
            page.read(cx).preferences.language,
            zenclash_core::LanguagePreference::En
        );
        assert!(!preferences.load().unwrap().system_proxy_enabled);
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, pid);
        assert!(
            page.read(cx)
                .application_commands
                .service_maintenance_preparation()
                .is_none()
        );
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn accepted_sidecar_does_not_repeat_local_choice_but_keeps_service_repair_reachable(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    fixture
        .core
        .record_startup_service_health(zenclash_core::ServiceHealthKind::Unavailable(
            "accepted local fallback".into(),
        ));
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("continue-local").is_none());
        assert!(fixture.core.run_state().sidecar_allowed);
        window.focus(&page.read(cx).focus_handle.clone(), cx);
        for _ in 0..12 {
            if window.find("repair-service").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("repair-service").focused(), Some(true));
        window.remove_window();
    })
    .unwrap();
}
