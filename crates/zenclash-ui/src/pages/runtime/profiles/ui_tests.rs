use crate::pages::runtime::test_support::{
    ControllerFixture, RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::component::WindowExt;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{TestAppContext, VisualTestContext, size};
use std::fs;

#[gpui_kit::test]
fn subscription_dialog_focuses_url_and_escape_preserves_draft(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let input = page.read(cx).profiles.forms.subscription_name.clone();
        assert!(window.has_active_dialog(cx));
        assert!(
            page.read(cx)
                .profiles
                .forms
                .subscription_url
                .focus_handle(cx)
                .is_focused(window)
        );
        input.update(cx, |input, cx| {
            input.set_value("unfinished draft", window, cx)
        });
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!page.read(cx).profiles.forms.adding_subscription);
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.click("toggle-add-subscription", cx);
        window.render_frame(cx);
        assert_eq!(
            page.read(cx)
                .profiles
                .forms
                .subscription_name
                .read(cx)
                .value(),
            "unfinished draft"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn subscription_validation_uses_notifications_without_repeating_on_updates(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::WindowExt;
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
        window.render_frame(cx);
        window.click("download-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let count = cx
        .update_window(window, |_, window, cx| {
            assert!(page.read(cx).profiles.forms.subscription_error.is_some());
            let count = window.notifications(cx).len();
            assert!(count > 0);
            page.update(cx, |_, cx| cx.notify());
            count
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.notifications(cx).len(), count);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn request_editor_focuses_name_and_escape_cancels_without_saving(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let remote = fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.profiles.add_remote(
            "Request settings fixture",
            &format!("{}/subscription", controller.url),
            "clash.meta",
        ))
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("inspect-profile:{}", remote.id), cx);
        window.render_frame(cx);
        window.click(format!("inspector-edit-profile-request:{}", remote.id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.find("profile-request-dialog");
        let input = page.read(cx).profiles.forms.request_name.clone();
        assert!(input.focus_handle(cx).is_focused(window));
        input.update(cx, |input, cx| {
            input.set_value("Unsaved request change", window, cx)
        });
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert!(page.read(cx).profiles.forms.editing_profile_id.is_none());
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.remove_window();
    })
    .unwrap();
    let persisted = fixture.profiles.load().unwrap();
    assert_eq!(
        persisted
            .profiles
            .iter()
            .find(|profile| profile.id == remote.id)
            .unwrap()
            .name,
        "Request settings fixture"
    );
}

#[gpui_kit::test]
fn cancelling_subscription_dialog_restores_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-add-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(page.read(cx).profiles.forms.adding_subscription);
        assert!(window.has_active_dialog(cx));
        let input = page
            .read(cx)
            .profiles
            .forms
            .subscription_name
            .focus_handle(cx);
        window.focus(&input, cx);
        window.render_frame(cx);
        assert!(input.is_focused(window));
        window.click("cancel-add-subscription", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!page.read(cx).profiles.forms.adding_subscription);
        assert!(page.read(cx).focus_handle.is_focused(window));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_yaml_preserves_edits_made_after_submission(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let submitted = "mixed-port: 7890\nmode: global\n".to_owned();
    let newer = format!("{submitted}# next edit\n");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("preview-overrides", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.preview().is_some());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("edit-source-profile", cx);
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            let id = page.profiles.catalog().active.clone().unwrap();
            let original = fs::read_to_string(&fixture.profile).unwrap();
            page.profiles.forms.yaml_editor.profile_id = Some(id.clone());
            page.profiles.forms.yaml_editor.original_yaml = Some(original.clone());
            page.profiles
                .forms
                .yaml_editor
                .input
                .update(cx, |input, cx| {
                    input.set_value(submitted.clone(), window, cx)
                });
            let token = page.begin_mutation(Page::Override).unwrap();
            page.profiles
                .forms
                .yaml_editor
                .input
                .update(cx, |input, cx| input.set_value(newer.clone(), window, cx));
            page.complete_profile_yaml_save(token, id, original, submitted.clone(), None, cx);
            assert_eq!(
                page.profiles.forms.yaml_editor.original_yaml.as_deref(),
                Some(submitted.as_str())
            );
            assert_eq!(
                page.profiles.forms.yaml_editor.input.read(cx).value(),
                newer
            );
            assert!(page.profiles.forms.yaml_editor.profile_id.is_some());
        });
        window.render_frame(cx);
        window.find("save-profile-yaml-edit");
        assert!(window.has_active_dialog(cx));
        window.click("cancel-profile-yaml-edit", cx);
        assert!(!window.has_active_dialog(cx));
        assert!(
            page.read(cx)
                .profiles
                .forms
                .yaml_editor
                .original_yaml
                .is_none()
        );
        assert_eq!(
            page.read(cx)
                .profiles
                .forms
                .yaml_editor
                .input
                .read(cx)
                .value(),
            ""
        );
        assert_eq!(
            fs::read_to_string(&fixture.profile).unwrap(),
            "mixed-port: 7890\nrules: [MATCH,DIRECT]\n"
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_yaml_after_navigation_still_invalidates_business_state(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Override);
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.config_inputs.is_loading()
    });
    let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = events.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, _: &ProfileActivated, _| {
            recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    });
    let submitted = "mixed-port: 7891\nmode: global\n".to_owned();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            assert_eq!(page.config_inputs.core.mixed_port.read(cx).value(), "7890");
            let id = page.profiles.catalog().active.clone().unwrap();
            let original = fs::read_to_string(&fixture.profile).unwrap();
            page.profiles.forms.yaml_editor.profile_id = Some(id.clone());
            page.profiles.forms.yaml_editor.original_yaml = Some(original.clone());
            page.profiles
                .forms
                .yaml_editor
                .input
                .update(cx, |input, cx| {
                    input.set_value(submitted.clone(), window, cx)
                });
            let token = page.begin_mutation(Page::Override).unwrap();
            let generation = page.config_inputs.read_generation();
            fs::write(&fixture.profile, &submitted).unwrap();
            page.switch_to(Page::Dns, cx);
            page.notice = Some("other page notice".into());
            page.complete_profile_yaml_save(
                token,
                id,
                original,
                submitted.clone(),
                Some((fixture.profile.clone(), fixture.core.generation())),
                cx,
            );
            assert!(page.config_inputs.read_generation() > generation);
            assert_eq!(page.notice.as_deref(), Some("other page notice"));
            assert!(!page.core_busy());
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.config_inputs.is_loading());
    cx.update(|cx| {
        let page = page.read(cx);
        assert_eq!(
            page.config_inputs.effective()["mixed-port"].as_u64(),
            Some(7891)
        );
        assert_eq!(page.config_inputs.core.mixed_port.read(cx).value(), "7891");
    });
    assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn subscription_interval_menu_cancels_without_writing_and_saves_keyboard_selection(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let remote = fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.profiles.add_remote(
            "Scheduled subscription",
            &format!("{}/subscription", controller.url),
            "clash.meta",
        ))
        .unwrap();
    fixture
        .profiles
        .set_update_policy(&remote.id, true, 1_440)
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let trigger = format!("profile-update-interval:{}", remote.id);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("inspect-profile:{}", remote.id), cx);
        window.render_frame(cx);
        window.click(trigger.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture
            .profiles
            .load()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.id == remote.id)
            .unwrap()
            .update_interval_minutes,
        1_440
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(trigger, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        page.profiles
            .catalog()
            .profiles
            .iter()
            .any(|profile| profile.id == remote.id && profile.update_interval_minutes == 360)
    });
    let catalog = fixture.profiles.load().unwrap();
    let saved = catalog
        .profiles
        .iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert!(saved.auto_update);
    assert_eq!(saved.update_interval_minutes, 360);
    assert_eq!(
        controller
            .apply_requests
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn subscription_download_policy_controls_preserve_credentials_and_schedule(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let url = format!("{}/subscription", controller.url);
    let remote = fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(
            fixture
                .profiles
                .add_remote("Policy subscription", &url, "clash.meta"),
        )
        .unwrap();
    let options = RemoteProfileOptions::new("Bearer test-fixture", false)
        .unwrap()
        .with_download_policy(45, true)
        .unwrap();
    fixture
        .profiles
        .set_remote_request_settings(
            &remote.id,
            &remote.name,
            &url,
            "clash.meta",
            options.clone(),
            Some("0 */6 * * *".into()),
        )
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("inspect-profile:{}", remote.id), cx);
        window.render_frame(cx);
        window.click(format!("profile-download-fallback:{}", remote.id), cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.profiles.catalog().profiles.iter().any(|profile| profile.id == remote.id && matches!(&profile.source, zenclash_core::ProfileSource::Remote { options, .. } if options.route() == RemoteProfileRoute::Direct)));
    let catalog = fixture.profiles.load().unwrap();
    let saved = catalog
        .profiles
        .iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert_eq!(saved.name, remote.name);
    assert_eq!(saved.update_cron.as_deref(), Some("0 */6 * * *"));
    let zenclash_core::ProfileSource::Remote {
        url: saved_url,
        user_agent,
        options: saved_options,
    } = &saved.source
    else {
        panic!("expected remote profile");
    };
    assert_eq!(saved_url, &url);
    assert_eq!(user_agent, "clash.meta");
    assert_eq!(
        saved_options,
        &options.with_route(RemoteProfileRoute::Direct)
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("profile-download-route:{}", remote.id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.profiles.catalog().profiles.iter().any(|profile| profile.id == remote.id && matches!(&profile.source, zenclash_core::ProfileSource::Remote { options, .. } if options.route() == RemoteProfileRoute::Mihomo)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("profile-download-fallback:{}", remote.id), cx);
        assert!(matches!(&page.read(cx).profiles.catalog().profiles.iter().find(|profile| profile.id == remote.id).unwrap().source, zenclash_core::ProfileSource::Remote { options, .. } if options.route() == RemoteProfileRoute::Mihomo));
        window.remove_window();
    })
    .unwrap();
    assert_eq!(
        controller
            .apply_requests
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[gpui_kit::test]
fn profile_more_menu_cancels_and_deletes_only_the_inactive_profile(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let source = fixture.root.join("secondary.yaml");
    fs::write(&source, "proxies: []\nrules: [MATCH,DIRECT]\n").unwrap();
    let secondary = fixture.profiles.import_local(&source).unwrap();
    let active = fixture.profiles.load().unwrap().active.unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    for id in [&active, &secondary.id] {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(format!("profile-more:{id}"), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            if id == &active {
                window.press("down", cx);
                window.press("enter", cx);
            }
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(fixture.profiles.load().unwrap().profiles.len(), 2);
    }
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("profile-more:{}", secondary.id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        page.profiles.catalog().profiles.len() == 1
    });
    let saved = fixture.profiles.load().unwrap();
    assert_eq!(saved.active.as_deref(), Some(active.as_str()));
    assert_eq!(saved.profiles[0].id, active);
    assert!(source.exists());
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn blurring_preserves_loaded_yaml_preview_until_the_window_is_hidden(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let _runtime_context = fixture.runtime.as_ref().unwrap().enter();
    let (window, page) = open(cx, &fixture, Page::Override);
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.persistent_loading && !page.loading && page.live_updates_enabled()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("preview-overrides", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| page.overrides.preview().is_some());

    VisualTestContext::from_window(window, cx).deactivate_window();
    assert!(cx.update(|cx| page.read(cx).live_updates_enabled()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find("copy-source-config");
        window.find("copy-effective-config");
        page.update(cx, |page, cx| page.set_window_visible(false, cx));
        window.render_frame(cx);
        assert!(window.try_find("copy-source-config").is_none());
        assert!(window.try_find("copy-effective-config").is_none());
        assert!(page.read(cx).overrides.preview().is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_delayed_yaml_save_does_not_replace_a_newer_profile(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        use zenclash_core::EffectiveConfigIntent;
        let controller = ControllerFixture::new(false);
        let fixture = Fixture::with_controller(controller.url.clone());
        let (window, page) = open(cx, &fixture, Page::Override);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        let original = fs::read_to_string(&fixture.profile).unwrap();
        // Listener admission is covered by core tests. Disable listeners here
        // so concurrent UI fixtures cannot collide while testing saved paths.
        let saved = "mixed-port: 0\nrules: [MATCH,DIRECT]\n".to_owned();
        let id = fixture.profiles.load().unwrap().active.unwrap();
        let token = cx
            .update_window(window, |_, window, cx| {
                page.update(cx, |page, cx| {
                    page.profiles.forms.yaml_editor.profile_id = Some(id.clone());
                    page.profiles.forms.yaml_editor.original_yaml = Some(original.clone());
                    page.profiles
                        .forms
                        .yaml_editor
                        .input
                        .update(cx, |input, cx| input.set_value(saved.clone(), window, cx));
                    page.begin_mutation(Page::Override).unwrap()
                })
            })
            .unwrap();
        fs::write(&fixture.profile, &saved).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let saved_path = fixture.profile.clone();
        let old_version = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: saved_path,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
                    .generation
            })
            .await
            .unwrap();
        let second_source = fixture.root.join("second.yaml");
        fs::write(&second_source, "mixed-port: 0\nrules: [MATCH,DIRECT]\n").unwrap();
        let second = fixture.profiles.import_local(second_source).unwrap();
        let second_path = fixture.profiles.activate(&second.id).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let applied_second = second_path.clone();
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: applied_second,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
            })
            .await
            .unwrap();
        assert!(fixture.core.generation() > old_version);
        {}
        let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = events.clone();
        let expected_path = second_path.clone();
        let expected_version = fixture.core.generation();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
                assert_eq!(event.path, expected_path);
                assert_eq!(event.runtime_version, expected_version);
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        let still_second = cx
            .update_window(window, |_, _, cx| {
                page.update(cx, |page, cx| {
                    {
                        page.profile_path = Some(second_path.clone());
                    }
                    page.complete_profile_yaml_save(
                        token,
                        id,
                        original,
                        saved,
                        Some((fixture.profile.clone(), old_version)),
                        cx,
                    );
                    // The completed disk write is still acknowledged, while runtime truth stays on B.
                    assert!(page.profiles.forms.yaml_editor.original_yaml.is_none());
                    page.profile_path.as_ref() == Some(&second_path)
                })
            })
            .unwrap();
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        assert!(
            still_second,
            "delayed A save replaced the newer active profile B"
        );
        assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

#[gpui_kit::test]
fn a_delayed_yaml_save_recovers_the_current_path_after_a_newer_mode_change(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        use zenclash_core::EffectiveConfigIntent;
        let controller = ControllerFixture::new(false);
        let fixture = Fixture::with_controller(controller.url.clone());
        let (window, page) = open(cx, &fixture, Page::Override);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        let original = fs::read_to_string(&fixture.profile).unwrap();
        // Listener admission is covered by core tests. Disable listeners here
        // so concurrent UI fixtures cannot collide while testing saved paths.
        let saved = "mixed-port: 0\nrules: [MATCH,DIRECT]\n".to_owned();
        let id = fixture.profiles.load().unwrap().active.unwrap();
        let token = cx
            .update_window(window, |_, window, cx| {
                page.update(cx, |page, cx| {
                    page.profiles.forms.yaml_editor.profile_id = Some(id.clone());
                    page.profiles.forms.yaml_editor.original_yaml = Some(original.clone());
                    page.profiles
                        .forms
                        .yaml_editor
                        .input
                        .update(cx, |input, cx| input.set_value(saved.clone(), window, cx));
                    page.begin_mutation(Page::Override).unwrap()
                })
            })
            .unwrap();
        fs::write(&fixture.profile, &saved).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let saved_path = fixture.profile.clone();
        let old_version = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: saved_path,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
                    .generation
            })
            .await
            .unwrap();
        let second_source = fixture.root.join("second.yaml");
        fs::write(&second_source, "mixed-port: 0\nrules: [MATCH,DIRECT]\n").unwrap();
        let second = fixture.profiles.import_local(second_source).unwrap();
        let second_path = fixture.profiles.activate(&second.id).unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let applied_second = second_path.clone();
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        EffectiveConfigIntent::ActivateProfile {
                            profile: applied_second,
                            overrides: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
            })
            .await
            .unwrap();
        assert!(fixture.core.generation() > old_version);
        {
            let session = fixture.core.clone();
            let controlled = fixture.controlled.clone();
            fixture
                .runtime
                .as_ref()
                .unwrap()
                .spawn(async move { session.set_mode(&controlled, "global").await.unwrap() })
                .await
                .unwrap();
        }
        let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = events.clone();
        let expected_path = second_path.clone();
        let expected_version = fixture.core.generation();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
                assert_eq!(event.path, expected_path);
                assert_eq!(event.runtime_version, expected_version);
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        let still_second = cx
            .update_window(window, |_, _, cx| {
                page.update(cx, |page, cx| {
                    {}
                    page.complete_profile_yaml_save(
                        token,
                        id,
                        original,
                        saved,
                        Some((fixture.profile.clone(), old_version)),
                        cx,
                    );
                    // The completed disk write is still acknowledged, while runtime truth stays on B.
                    assert!(page.profiles.forms.yaml_editor.original_yaml.is_none());
                    page.profile_path.as_ref() == Some(&second_path)
                })
            })
            .unwrap();
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        assert!(
            still_second,
            "delayed A save replaced the newer active profile B"
        );
        assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

#[gpui_kit::test]
fn a_saved_profile_confirmation_uses_a_separate_button_and_preserves_data_on_failure(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let before = fs::read(&fixture.profile).unwrap();
    let version = fixture.core.generation();
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            // Typed committed outcome fixture; this is UI recovery coverage, not native service validation.
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
        assert!(window.try_find("reapply-profile-recovery").is_none());
        window.find("confirm-service-profile");
        window.click("confirm-service-profile", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.profiles.pending_finalization(), Some(version));
        assert!(page.profiles.recovery().is_none());
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert_eq!(fixture.core.generation(), version);
        assert_eq!(
            fixture.profiles.active_path().unwrap(),
            Some(fixture.profile.clone())
        );
        assert_eq!(fs::read(&fixture.profile).unwrap(), before);
        window.render_frame(cx);
        window.find("confirm-service-profile");
        assert!(window.try_find("reapply-profile-recovery").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_recovery_button_reapplies_the_recorded_profile_and_clears_uncertainty(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        let controller = ControllerFixture::new(true);
        let fixture = Fixture::with_controller(controller.url.clone());
        let source = fixture.root.join("candidate.yaml");
        fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let candidate = fixture.profiles.import_local(source).unwrap();
        let (window, page) = open(cx, &fixture, Page::Profiles);
        fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
        let commands = cx.update(|cx| page.read(cx).application_commands.clone());
        let store = fixture.profiles.clone();
        let controlled = fixture.controlled.clone();
        let failure = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                commands
                    .activate(store, controlled, candidate.id)
                    .await
                    .err()
                    .unwrap()
                    .to_string()
            })
            .await
            .unwrap();
        let attempted_version = fixture.core.generation();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| page.report_tray_profile_error(&failure, cx));
            assert!(page.read(cx).profiles.recovery().is_some());
            window.render_frame(cx);
            window.find("reapply-profile-recovery");
            {
                window.click("reapply-profile-recovery", cx);
            }
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.core_busy() && page.core_session.generation() > attempted_version
        });
        cx.update(|cx| {
            assert_eq!(page.read(cx).profile_path.as_ref(), Some(&fixture.profile));
            assert!(
                page.read(cx).profiles.recovery().is_none(),
                "an accepted reload left a stale recovery card"
            );
        });
        assert_eq!(
            fixture.core.committed_profile_snapshot().profile_path,
            Some(fixture.profile.clone())
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("reapply-profile-recovery").is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn a_manual_reload_clears_the_recovery_card_after_runtime_acceptance(cx: &mut TestAppContext) {
    cx.foreground_executor().clone().block_test(async {
        let controller = ControllerFixture::new(true);
        let fixture = Fixture::with_controller(controller.url.clone());
        let source = fixture.root.join("candidate.yaml");
        fs::write(&source, "mixed-port: 7891\nrules: [MATCH,REJECT]\n").unwrap();
        let candidate = fixture.profiles.import_local(source).unwrap();
        let (window, page) = open(cx, &fixture, Page::Profiles);
        fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
        let commands = cx.update(|cx| page.read(cx).application_commands.clone());
        let store = fixture.profiles.clone();
        let controlled = fixture.controlled.clone();
        let failure = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                commands
                    .activate(store, controlled, candidate.id)
                    .await
                    .err()
                    .unwrap()
                    .to_string()
            })
            .await
            .unwrap();
        let attempted_version = fixture.core.generation();
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| page.report_tray_profile_error(&failure, cx));
            assert!(page.read(cx).profiles.recovery().is_some());
            window.render_frame(cx);
            window.find("reapply-profile-recovery");
            {
                window.scroll(
                    "reapply-profile-recovery",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1000.))),
                    cx,
                );
                window.render_frame(cx);
                window.click("reload-profile", cx);
            }
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            !page.core_busy() && page.core_session.generation() > attempted_version
        });
        cx.update(|cx| {
            assert_eq!(page.read(cx).profile_path.as_ref(), Some(&fixture.profile));
            assert!(
                page.read(cx).profiles.recovery().is_none(),
                "an accepted reload left a stale recovery card"
            );
        });
        assert_eq!(
            fixture.core.committed_profile_snapshot().profile_path,
            Some(fixture.profile.clone())
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("reapply-profile-recovery").is_none());
            window.remove_window();
        })
        .unwrap();
    });
}

#[gpui_kit::test]
fn adding_a_remote_profile_with_a_lost_response_displays_the_recovery_card(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| page.open_subscription_dialog(window, cx));
        page.update(cx, |page, cx| {
            page.profiles.forms.adding_subscription = true;
            page.profiles.forms.subscription_route = zenclash_core::RemoteProfileRoute::Direct;
            page.profiles
                .forms
                .subscription_name
                .update(cx, |input, cx| {
                    input.set_value("remote fixture", window, cx)
                });
            page.profiles
                .forms
                .subscription_url
                .update(cx, |input, cx| {
                    input.set_value(format!("{}/subscription", controller.url), window, cx)
                });
        });
        window.render_frame(cx);
        window.click("download-subscription", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy() && page.profiles.forms.subscription_error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        assert!(
            page.read(cx).profiles.recovery().is_some(),
            "a remote runtime uncertainty was hidden from the profile page"
        );
        window.render_frame(cx);
        window.find("reapply-profile-recovery");
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn update_all_subscriptions_downloads_each_remote_once_and_keeps_active_profile(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    for name in ["First remote", "Second remote"] {
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .block_on(fixture.profiles.add_remote(
                name,
                &format!("{}/subscription", controller.url),
                "clash.meta",
            ))
            .unwrap();
    }
    let active = fixture.profiles.load().unwrap().active;
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading && !page.loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("update-all-profiles", cx);
        window.render_frame(cx);
        window.click("update-all-profiles", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.notice.is_some());
    assert_eq!(controller.subscription_requests.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.profiles.load().unwrap().active, active);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn request_modal_validates_and_saves_without_resetting_download_policy(cx: &mut TestAppContext) {
    let controller = ControllerFixture::new(false);
    let fixture = Fixture::with_controller(controller.url.clone());
    let remote = fixture
        .runtime
        .as_ref()
        .unwrap()
        .block_on(fixture.profiles.add_remote(
            "Request save fixture",
            &format!("{}/subscription", controller.url),
            "clash.meta",
        ))
        .unwrap();
    fixture
        .profiles
        .set_remote_request_settings(
            &remote.id,
            &remote.name,
            &format!("{}/subscription", controller.url),
            "clash.meta",
            RemoteProfileOptions::new("Bearer fixture-secret", false)
                .unwrap()
                .with_download_policy(17, true)
                .unwrap(),
            Some("0 */6 * * *".into()),
        )
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.begin_edit_remote_profile(remote.id.clone(), window, cx);
            page.profiles
                .forms
                .request_url
                .update(cx, |input, cx| input.set_value("invalid-url", window, cx));
            page.save_remote_profile_settings(cx);
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.core_busy() && page.error.is_some());
    cx.update_window(window, |_, window, cx| {
        assert!(window.has_active_dialog(cx));
        page.update(cx, |page, cx| {
            page.profiles.forms.request_url.update(cx, |input, cx| {
                input.set_value(format!("{}/subscription", controller.url), window, cx)
            });
            page.profiles
                .forms
                .request_name
                .update(cx, |input, cx| input.set_value("Saved request", window, cx));
            page.save_remote_profile_settings(cx);
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.core_busy() && page.profiles.forms.editing_profile_id.is_none()
    });
    cx.update_window(window, |_, window, cx| {
        assert!(!window.has_active_dialog(cx));
        window.remove_window();
    })
    .unwrap();
    let saved = fixture.profiles.load().unwrap();
    let saved = saved
        .profiles
        .iter()
        .find(|profile| profile.id == remote.id)
        .unwrap();
    assert_eq!(saved.name, "Saved request");
    assert_eq!(saved.update_cron.as_deref(), Some("0 */6 * * *"));
    let zenclash_core::ProfileSource::Remote { options, .. } = &saved.source else {
        panic!("remote source changed")
    };
    assert_eq!(options.download_timeout_seconds, 17);
    assert!(options.fixed_update_interval);
    assert_eq!(
        options.authorization.as_ref().unwrap().expose_secret(),
        "Bearer fixture-secret"
    );
}

#[gpui_kit::test]
fn subscription_form_preserves_geometry_draft_and_focus_across_rem_zoom(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (window, page) = open(cx, &fixture, Page::Profiles);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let mut wide_dialog_ratios = Vec::new();
    let mut submitted_draft = String::new();
    for font in [12., 16., 20.] {
        cx.update(|cx| gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(font)));
        for dimensions in [size(px(720.), px(560.)), size(px(1280.), px(820.))] {
            cx.simulate_window_resize(window, dimensions);
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click("toggle-add-subscription", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.rem_size(), px(font));
                assert!(window.has_active_dialog(cx));
                let field = window.find("subscription-user-agent-field").bounds();
                let dialog = window.within("dialog").find(0usize).bounds();
                assert!(field.size.width > px(0.));
                if dimensions.width == px(1280.) {
                    wide_dialog_ratios.push(f32::from(dialog.size.width) / font);
                }
                assert!(field.left() >= dialog.left() && field.right() <= dialog.right());
                assert!(dialog.left() >= px(0.) && dialog.right() <= window.viewport_size().width);
                assert!(
                    page.read(cx)
                        .profiles
                        .forms
                        .subscription_url
                        .focus_handle(cx)
                        .is_focused(window)
                );
                window.input("https://example.com/订阅", cx);
                submitted_draft = page
                    .read(cx)
                    .profiles
                    .forms
                    .subscription_url
                    .read(cx)
                    .value()
                    .to_string();
                assert!(submitted_draft.contains("订阅"));
                window.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(!window.has_active_dialog(cx));
                assert!(page.read(cx).focus_handle.is_focused(window));
                assert_eq!(
                    page.read(cx)
                        .profiles
                        .forms
                        .subscription_url
                        .read(cx)
                        .value()
                        .as_str(),
                    submitted_draft
                );
            })
            .unwrap();
        }
    }
    assert_eq!(wide_dialog_ratios.len(), 3);
    assert!(
        wide_dialog_ratios
            .windows(2)
            .all(|ratios| (ratios[0] - ratios[1]).abs() <= 0.1),
        "dialog width did not preserve its relative size: {wide_dialog_ratios:?}"
    );
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
