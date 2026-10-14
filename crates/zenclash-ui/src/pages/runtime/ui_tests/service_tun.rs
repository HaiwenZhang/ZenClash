use crate::pages::runtime::test_support::{
    RuntimePageFixture as Fixture, open_service_page, open_service_tun, owned_ui_children,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::fs;

#[gpui_kit::test]
fn home_service_feedback_saved_commit_has_keyboard_details_without_resubmission(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let version = fixture.core.generation();
    let bytes = fs::read(&fixture.profile).unwrap();
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let navigations = std::rc::Rc::new(std::cell::Cell::new(0));
    let received = navigations.clone();
    let target = page.downgrade();
    cx.update(|cx| {
        cx.on_action(move |_: &crate::app::NavigateTun, cx| {
            received.set(received.get() + 1);
            let _ = target.update(cx, |page, cx| page.switch_to(Page::Tun, cx));
        });
    });
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.application_commands
                .publish_test_outcome(
                    zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        source_version: (&record).into(),
                        profile: record.clone(),
                        path: fixture.profile.clone(),
                        cause: zenclash_core::ProfileApplicationError::Task(
                            "commit reply lost".into(),
                        ),
                        runtime_version: version,
                    },
                )
                .unwrap();
            page.synchronize_profile_recovery();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("home-service-pending").label(),
            Some(zenclash_i18n::text("core_page.service.pending").as_str())
        );
        assert_eq!(
            window.find("home-tun-disabled-reason").label(),
            Some(zenclash_i18n::text("tun.control.confirm_service").as_str())
        );
        window.click("home-tun", cx);
        window.click("home-tun", cx);
        assert!(page.read(cx).home.action_error.is_none());
        assert_eq!(
            page.read(cx)
                .application_commands
                .service_state()
                .unwrap()
                .phase(),
            zenclash_core::ServicePhase::Idle
        );
        assert_eq!(
            page.read(cx).application_commands.pending_finalization(),
            Some(version)
        );
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            if window.find("home-open-tun").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("home-open-tun").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            navigations.get(),
            1,
            "details must dispatch the application navigation action"
        );
        assert_eq!(page.read(cx).page, Page::Tun);
        window.find("confirm-service-tun");
        page.update(cx, |page, cx| {
            let token = page.page_task_token_for(Page::Tun);
            page.replace_page_data(
                token,
                PageSnapshotData::Config(RuntimeConfig::default()),
                cx,
            );
        });
        window.render_frame(cx);
        window.click("tun-enable", cx);
        assert!(
            page.read(cx)
                .application_commands
                .pending_finalization()
                .is_some()
        );
        assert_eq!(
            window.find("tun-control-disabled-reason").label(),
            Some(zenclash_i18n::text("tun.control.confirm_service").as_str())
        );
        page.update(cx, |page, cx| page.switch_to(Page::Home, cx));
        window.render_frame(cx);
        window.find("home-service-pending");
        assert_eq!(
            page.read(cx).application_commands.pending_finalization(),
            Some(version)
        );
        assert_eq!(fixture.core.generation(), version);
        assert_eq!(fs::read(&fixture.profile).unwrap(), bytes);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_immediate_rejection_survives_page_refresh(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let generation = fixture.core.generation();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("home-tun", cx);
        assert!(!window.has_active_dialog(cx));
        let expected = zenclash_i18n::text("core_page.service.unsupported");
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(expected.as_str())
        );
        page.update(cx, |page, cx| page.refresh(cx));
        window.render_frame(cx);
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(
            window.find("home-capture-error").label(),
            Some(expected.as_str())
        );
        assert!(!page.read(cx).core_busy());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_consent_cancel_preserves_local_owner_and_configuration(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = local.snapshot().pid;
    let configuration = fs::read(&local.launch_config().config_file).unwrap();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            if window.find("home-tun").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("home-tun").focused(), Some(true));
        page.update(cx, |page, _| {
            page.home.action_error = Some(zenclash_i18n::text("core_page.service.unsupported"));
        });
        window.click("home-tun", cx);
        assert!(page.read(cx).home.action_error.is_none());
        assert!(
            window.has_active_dialog(cx),
            "the real Home switch must request service consent before touching the local core"
        );
        window.click("home-tun", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(window.find("home-tun").focused(), Some(true));
        window.press("space", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert_eq!(
            window.find("home-tun").label(),
            Some(zenclash_i18n::text("home.controls.tun").as_str()),
        );
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(local.snapshot().pid, pid);
        assert_eq!(
            fs::read(&local.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(!page.read(cx).core_busy());
        assert!(
            !page
                .read(cx)
                .application_commands
                .service_state()
                .unwrap()
                .is_busy()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn home_service_tun_confirmation_rejects_replaced_owner_without_local_grant(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_page(cx, &fixture, Page::Home);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("home-tun", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let pid = replacement.snapshot().pid;
    let configuration = fs::read(&replacement.launch_config().config_file).unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(page.read(cx).core_session.generation(), generation);
        assert_eq!(
            page.read(cx).application_commands.session().generation(),
            generation
        );
        assert!(!page.read(cx).persistent_loading);
        assert!(!page.read(cx).core_busy());
        window.click("ok", cx);
        assert!(
            !window.has_active_dialog(cx),
            "the real confirm button must submit and close its dialog"
        );
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy() && page.home.action_error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).home.action_error.as_deref(),
            Some(zenclash_i18n::text("core_page.service.stale").as_str())
        );
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, pid);
        assert_eq!(
            fs::read(&replacement.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(!window.has_active_dialog(cx));
        assert!(
            page.read(cx)
                .application_commands
                .pending_finalization()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_tun_consent_escape_keeps_the_real_local_owner_and_configuration(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, _unused] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let configuration = fs::read(&local.launch_config().config_file).unwrap();
    let pid = local.snapshot().pid.unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..8 {
            if window.find("enable-service-tun").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("enable-service-tun").focused(), Some(true));
        window.click("enable-service-tun", cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(window.find("enable-service-tun").focused(), Some(true));
        window.press("enter", cx);
        assert!(
            window.has_active_dialog(cx),
            "keyboard activation must reach the same consent"
        );
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(local.snapshot().pid, Some(pid));
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(
            fs::read(&local.launch_config().config_file).unwrap(),
            configuration
        );
        assert!(
            !page
                .read(cx)
                .application_commands
                .service_state()
                .unwrap()
                .is_busy()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn service_tun_confirmation_cannot_authorize_a_core_switched_while_dialog_open(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let [local, replacement] = owned_ui_children(&fixture);
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(local))
        .unwrap();
    let (window, page) = open_service_tun(cx, &fixture);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("enable-service-tun", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
    runtime
        .block_on(fixture.core.switch_to_process(replacement.clone()))
        .unwrap();
    let generation = fixture.core.generation();
    let replacement_pid = replacement.snapshot().pid.unwrap();
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
        assert_eq!(fixture.core.generation(), generation);
        assert_eq!(replacement.snapshot().pid, Some(replacement_pid));
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
}
