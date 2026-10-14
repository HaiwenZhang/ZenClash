use super::*;
use crate::app::{SetDarkTheme, SetLightTheme, test_support::app_test_services};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, px, size};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AppearancePreference, LanguagePreference,
    TrayDisplayPreference,
};

fn open_app(
    fixture: &crate::pages::runtime::test_support::RuntimePageFixture,
    store: Option<AppPreferencesStore>,
    cx: &mut TestAppContext,
) -> (Entity<ZenClashApp>, WindowHandle<Root>) {
    let services = app_test_services(fixture);
    cx.executor().allow_parking();
    cx.update(crate::app::init);
    let mut owner = None;
    let window = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
        let preferences = services.preferences.clone();
        let app =
            cx.new(|cx| ZenClashApp::new(services, None, None, store, preferences, window, cx));
        owner = Some(app.clone());
        Root::new(app, window, cx)
    });
    (owner.unwrap(), window)
}

fn finish_writes(owner: &Entity<ZenClashApp>, cx: &mut TestAppContext) {
    let task = cx.update(|cx| owner.update(cx, |app, _| app.preferences_save_task.take()));
    cx.foreground_executor().clone().block_test(async {
        if let Some(task) = task {
            task.await.unwrap();
        }
    });
    cx.run_until_parked();
}

#[gpui_kit::test]
fn failed_menu_save_is_visible_and_retry_commits_only_the_selected_field(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    let blocked_parent = services
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("menu-save-parent");
    std::fs::write(&blocked_parent, "not a directory").unwrap();
    let path = blocked_parent.join("preferences.json");
    let store = AppPreferencesStore::new(&path);
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);

    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            app.on_set_dark_theme(&SetDarkTheme, window, cx)
        });
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Dark
        );
    })
    .unwrap();
    finish_writes(&owner, cx);
    assert!(
        cx.update(|cx| owner
            .read(cx)
            .menu_preference_saves
            .failure_message()
            .is_some()),
        "a failed filesystem save must return an unsaved status to the application owner"
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("menu-preference-save-failure").visible());
        assert!(window.find("retry-menu-preferences").visible());
    })
    .unwrap();
    assert!(!path.exists());

    std::fs::remove_file(&blocked_parent).unwrap();
    let externally_saved = AppPreferences {
        language: LanguagePreference::En,
        ..Default::default()
    };
    store.save(&externally_saved).unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("retry-menu-preferences", cx);
    })
    .unwrap();
    finish_writes(&owner, cx);
    let saved = store.load().unwrap();
    assert_eq!(saved.appearance, AppearancePreference::Dark);
    assert_eq!(saved.language, LanguagePreference::En);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("menu-preference-save-failure").is_none());
        assert!(
            owner
                .read(cx)
                .menu_preference_saves
                .failure_message()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn missing_preference_store_reports_unsaved_tray_selection(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let (owner, window) = open_app(&fixture, None, cx);
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            app.set_tray_display(TrayDisplayPreference::Icon, cx)
        });
        window.render_frame(cx);
        assert!(window.find("menu-preference-save-failure").visible());
        assert_eq!(
            owner.read(cx).preferences.tray_display,
            TrayDisplayPreference::Icon
        );
        assert!(
            owner
                .read(cx)
                .menu_preference_saves
                .failure_message()
                .is_some()
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn unsaved_menu_preference_can_be_retried_with_keyboard(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    let path = services
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("keyboard-menu-preferences.json");
    let store = AppPreferencesStore::new(path);
    let (owner, window) = open_app(&fixture, None, cx);
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            app.on_set_dark_theme(&SetDarkTheme, window, cx);
            app.preferences_store = Some(store.clone());
        });
        window.render_frame(cx);
        for _ in 0..30 {
            if window.find("retry-menu-preferences").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("retry-menu-preferences").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    finish_writes(&owner, cx);
    assert_eq!(store.load().unwrap().appearance, AppearancePreference::Dark);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("menu-preference-save-failure").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn queued_menu_choices_keep_latest_preview_and_save_order(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let services = app_test_services(&fixture);
    let path = services
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("menu-preferences.json");
    let store = AppPreferencesStore::new(path);
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);
    let (release, waiting) = tokio::sync::oneshot::channel();
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            app.preferences_save_task = Some(app.runtime.spawn(async move {
                waiting.await.unwrap();
            }));
            app.on_set_dark_theme(&SetDarkTheme, window, cx);
            app.on_set_light_theme(&SetLightTheme, window, cx);
            app.set_tray_display(TrayDisplayPreference::Icon, cx);
        });
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Light
        );
    })
    .unwrap();
    assert_eq!(
        store.load().unwrap().appearance,
        AppearancePreference::System
    );
    release.send(()).unwrap();
    finish_writes(&owner, cx);
    let saved = store.load().unwrap();
    assert_eq!(saved.appearance, AppearancePreference::Light);
    assert_eq!(saved.tray_display, TrayDisplayPreference::Icon);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Light
        );
        assert!(
            owner
                .read(cx)
                .menu_preference_saves
                .failure_message()
                .is_none()
        );
        window.remove_window();
    })
    .unwrap();
}

#[test]
fn older_success_and_failure_cannot_change_newer_intents_save_status() {
    let mut saves = MenuPreferenceSaves::default();
    let old_failure = saves.begin(MenuPreferenceChange::Appearance(AppearancePreference::Dark));
    let new_success = saves.begin(MenuPreferenceChange::Appearance(
        AppearancePreference::Light,
    ));
    assert!(saves.complete(new_success, Ok(())));
    assert!(!saves.complete(old_failure, Err("old write failed".into())));
    assert!(saves.failure_message().is_none());

    let old_success = saves.begin(MenuPreferenceChange::Appearance(AppearancePreference::Dark));
    let new_failure = saves.begin(MenuPreferenceChange::Appearance(
        AppearancePreference::Light,
    ));
    assert!(saves.complete(new_failure, Err("latest write failed".into())));
    assert!(!saves.complete(old_success, Ok(())));
    assert!(
        saves
            .failure_message()
            .unwrap()
            .contains("latest write failed")
    );
}

#[test]
fn another_field_success_cannot_hide_a_failure_and_retry_retains_latest_value() {
    let mut saves = MenuPreferenceSaves::default();
    let failed = saves.begin(MenuPreferenceChange::TrayDisplay(
        TrayDisplayPreference::Icon,
    ));
    saves.complete(failed, Err("disk full".into()));
    let unrelated = saves.begin(MenuPreferenceChange::TrayVisibility(false));
    saves.complete(unrelated, Ok(()));
    assert!(saves.failure_message().unwrap().contains("disk full"));
    let replacement = saves.begin(MenuPreferenceChange::TrayDisplay(
        TrayDisplayPreference::Traffic,
    ));
    assert!(saves.has_pending_retry());
    assert!(saves.retry_changes().is_empty());
    saves.complete(replacement, Err("still full".into()));
    assert!(matches!(
        saves.retry_changes().as_slice(),
        [MenuPreferenceChange::TrayDisplay(
            TrayDisplayPreference::Traffic
        )]
    ));
}

#[gpui_kit::test]
fn a_restore_event_received_after_a_new_menu_commit_adopts_current_preferences(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("restore-menu-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let event = crate::pages::runtime::PreferencesRestored::restored(store.observe().unwrap());
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.update_menu_preference(
                MenuPreferenceChange::Appearance(AppearancePreference::Dark),
                cx,
            )
        })
    });
    finish_writes(&owner, cx);
    assert_eq!(store.load().unwrap().appearance, AppearancePreference::Dark);
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.accept_preferences_event(&event, cx);
            assert_eq!(app.preferences.appearance, AppearancePreference::Dark);
        })
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert!(owner.read(cx).preferences_reconciliation_read.is_none());
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Dark
        );
        assert_eq!(
            owner
                .read(cx)
                .runtime_page
                .read(cx)
                .presented_preferences()
                .appearance,
            AppearancePreference::Dark
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn restore_adoption_preserves_pending_and_failed_menu_previews(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("restore-preview-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let event = crate::pages::runtime::PreferencesRestored::restored(store.observe().unwrap());
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);
    cx.update_window(window.into(), |_, _window, cx| {
        owner.update(cx, |app, cx| {
            let appearance = app
                .menu_preference_saves
                .begin(MenuPreferenceChange::Appearance(AppearancePreference::Dark));
            let _pending = app
                .menu_preference_saves
                .begin(MenuPreferenceChange::TrayDisplay(
                    TrayDisplayPreference::Icon,
                ));
            assert!(
                app.menu_preference_saves
                    .complete(appearance, Err("fixture unsaved appearance".into()))
            );
            app.accept_preferences_event(&event, cx);
        });
    })
    .unwrap();
    // Busy data publication can defer adoption to the existing reconciliation worker.
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                return;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        panic!("restored preference previews did not finish reconciliation");
    });
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            assert_eq!(app.preferences.appearance, AppearancePreference::Dark);
            assert_eq!(app.preferences.tray_display, TrayDisplayPreference::Icon);
            assert!(
                app.menu_preference_saves
                    .failure_message()
                    .unwrap()
                    .contains("fixture unsaved appearance")
            );
            assert!(
                app.menu_preference_saves
                    .tray_display
                    .as_ref()
                    .unwrap()
                    .pending
            );
            assert_eq!(
                app.runtime_page.read(cx).presented_preferences().appearance,
                AppearancePreference::Dark
            );
            assert_eq!(
                app.runtime_page
                    .read(cx)
                    .presented_preferences()
                    .tray_display,
                TrayDisplayPreference::Icon
            );
        });
        assert_eq!(
            store.load().unwrap().appearance,
            AppearancePreference::System
        );
        assert_eq!(
            store.load().unwrap().tray_display,
            TrayDisplayPreference::Traffic
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_queued_page_restore_event_checks_a_commit_made_after_emission(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("queued-restore-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let source = store.observe().unwrap();
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);
    let page = cx.update(|cx| owner.read(cx).runtime_page.clone());
    cx.update(|cx| {
        page.update(cx, |_, cx| {
            cx.emit(crate::pages::runtime::PreferencesRestored::restored(source));
        });
        // GPUI has not consumed pending effects yet; commit after the actual page emit.
        store
            .update(|preferences| preferences.appearance = AppearancePreference::Dark)
            .unwrap();
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| {
                owner.read(cx).preferences.appearance == AppearancePreference::Dark
                    && owner.read(cx).preferences_reconciliation_read.is_none()
            }) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Dark
        );
        assert_eq!(
            page.read(cx).presented_preferences().appearance,
            AppearancePreference::Dark
        );
        assert!(owner.read(cx).preferences_reconciliation_read.is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn menu_completion_replaces_a_preview_with_the_preferences_committed_by_restore(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("completed-preview-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let (owner, window) = open_app(&fixture, Some(store.clone()), cx);
    let receipt = cx.update(|cx| {
        owner.update(cx, |app, _| {
            app.menu_preference_saves
                .begin(MenuPreferenceChange::Appearance(AppearancePreference::Dark))
        })
    });
    store
        .update(|preferences| preferences.appearance = AppearancePreference::Dark)
        .unwrap();
    // A restore replaced the saved value before the foreground completion was consumed.
    store
        .update(|preferences| preferences.appearance = AppearancePreference::Light)
        .unwrap();
    let event = crate::pages::runtime::PreferencesRestored::restored(store.observe().unwrap());
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.accept_preferences_event(&event, cx);
            assert_eq!(app.preferences.appearance, AppearancePreference::Dark);
            app.complete_menu_preference(receipt, Ok(()), cx);
        })
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| {
                owner.read(cx).preferences.appearance == AppearancePreference::Light
                    && owner.read(cx).preferences_reconciliation_read.is_none()
            }) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Light
        );
        assert_eq!(
            owner
                .read(cx)
                .runtime_page
                .read(cx)
                .presented_preferences()
                .appearance,
            AppearancePreference::Light
        );
        assert_eq!(
            store.load().unwrap().appearance,
            AppearancePreference::Light
        );
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_foreign_restore_event_does_not_cancel_current_preference_reconciliation(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let root = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let store = AppPreferencesStore::new(root.join("owned-reconciliation-preferences.json"));
    let foreign = AppPreferencesStore::new(root.join("foreign-reconciliation-preferences.json"));
    store.save(&AppPreferences::default()).unwrap();
    foreign.save(&AppPreferences::default()).unwrap();
    let source = store.observe().unwrap();
    store
        .update(|preferences| preferences.appearance = AppearancePreference::Light)
        .unwrap();
    let foreign_event =
        crate::pages::runtime::PreferencesRestored::restored(foreign.observe().unwrap());
    let (owner, window) = open_app(&fixture, Some(store), cx);
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.accept_preferences_event(
                &crate::pages::runtime::PreferencesRestored::restored(source),
                cx,
            );
            let identity = app.preferences_reconciliation_read.clone().unwrap();
            app.accept_preferences_event(&foreign_event, cx);
            assert!(std::sync::Arc::ptr_eq(
                &app.preferences_reconciliation_read
                    .as_ref()
                    .unwrap()
                    .identity,
                &identity.identity
            ));
        })
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Light
        );
        assert!(owner.read(cx).preferences_reconciliation_read.is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn observed_capture_preferences_preserve_unresolved_menu_previews_and_current_fields(
    cx: &mut TestAppContext,
) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("capture-preview-preferences.json");
    let store = AppPreferencesStore::new(path);
    let saved = store
        .update(|preferences| preferences.system_proxy_host = "capture-host".into())
        .unwrap();
    let source = store.observe().unwrap();
    let (owner, window) = open_app(&fixture, Some(store), cx);
    cx.update_window(window.into(), |_, _window, cx| {
        owner.update(cx, |app, cx| {
            app.preferences.language = LanguagePreference::En;
            let failed = app
                .menu_preference_saves
                .begin(MenuPreferenceChange::Appearance(AppearancePreference::Dark));
            app.menu_preference_saves
                .complete(failed, Err("capture must keep this error".into()));
            app.accept_preferences_event(
                &crate::pages::runtime::PreferencesRestored::observed(
                    source,
                    crate::pages::runtime::PreferenceScope::SystemProxy,
                ),
                cx,
            );
        });
    })
    .unwrap();
    // Observed capture preferences may be adopted by the existing asynchronous reread.
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                return;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
        panic!("capture preferences did not finish reconciliation");
    });
    cx.update_window(window.into(), |_, window, cx| {
        owner.update(cx, |app, cx| {
            assert_eq!(app.preferences.system_proxy_host, saved.system_proxy_host);
            assert_eq!(app.preferences.language, LanguagePreference::En);
            assert_eq!(app.preferences.appearance, AppearancePreference::Dark);
            assert!(
                app.menu_preference_saves
                    .failure_message()
                    .unwrap()
                    .contains("capture must keep")
            );
            let preferences = app.preferences.clone();
            let previews = app.menu_preference_saves.previews();
            app.runtime_page.update(cx, |page, cx| {
                page.preferences_restored_from_app(preferences, previews, cx)
            });
            assert_eq!(
                app.runtime_page.read(cx).presented_preferences().appearance,
                AppearancePreference::Dark
            );
        });
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn stale_capture_preference_reconciliation_preserves_its_partial_scope(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("stale-capture-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let source = store.observe().unwrap();
    store
        .update(|preferences| preferences.system_proxy_host = "capture.example".into())
        .unwrap();
    let (owner, window) = open_app(&fixture, Some(store), cx);
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.preferences.language = LanguagePreference::En;
            app.accept_preferences_event(
                &crate::pages::runtime::PreferencesRestored::observed(
                    source,
                    crate::pages::runtime::PreferenceScope::SystemProxy,
                ),
                cx,
            );
        })
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.system_proxy_host,
            "capture.example"
        );
        assert_eq!(owner.read(cx).preferences.language, LanguagePreference::En);
        assert_eq!(
            owner
                .read(cx)
                .runtime_page
                .read(cx)
                .presented_preferences()
                .system_proxy_host,
            "capture.example"
        );
        assert!(owner.read(cx).preferences_reconciliation_read.is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_partial_reconciliation_retains_an_already_pending_full_restore(cx: &mut TestAppContext) {
    let fixture = crate::pages::runtime::test_support::RuntimePageFixture::new();
    let path = app_test_services(&fixture)
        .profile_path
        .unwrap()
        .parent()
        .unwrap()
        .join("combined-reconciliation-preferences.json");
    let store = AppPreferencesStore::new(path);
    store.save(&AppPreferences::default()).unwrap();
    let before_restore = store.observe().unwrap();
    store
        .update(|preferences| preferences.appearance = AppearancePreference::Light)
        .unwrap();
    let before_capture = store.observe().unwrap();
    store
        .update(|preferences| preferences.system_proxy_host = "combined.example".into())
        .unwrap();
    let (owner, window) = open_app(&fixture, Some(store), cx);
    cx.update(|cx| {
        owner.update(cx, |app, cx| {
            app.accept_preferences_event(
                &crate::pages::runtime::PreferencesRestored::restored(before_restore),
                cx,
            );
            assert!(app.preferences_reconciliation_read.is_some());
            app.accept_preferences_event(
                &crate::pages::runtime::PreferencesRestored::observed(
                    before_capture,
                    crate::pages::runtime::PreferenceScope::SystemProxy,
                ),
                cx,
            );
        })
    });
    cx.foreground_executor().clone().block_test(async {
        for _ in 0..200 {
            if cx.update(|cx| owner.read(cx).preferences_reconciliation_read.is_none()) {
                break;
            }
            let runtime = cx.update(|cx| owner.read(cx).runtime.clone());
            runtime
                .spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                })
                .await
                .unwrap();
        }
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            owner.read(cx).preferences.appearance,
            AppearancePreference::Light
        );
        assert_eq!(
            owner.read(cx).preferences.system_proxy_host,
            "combined.example"
        );
        assert!(owner.read(cx).preferences_reconciliation_read.is_none());
        window.remove_window();
    })
    .unwrap();
}
