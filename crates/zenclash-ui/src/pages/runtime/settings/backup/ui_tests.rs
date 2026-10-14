use crate::pages::runtime::test_support::{
    ControllerFixture, RuntimePageFixture as Fixture, open_runtime_page_window as open,
};
use crate::pages::runtime::*;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::{fs, time::Duration};

fn retain_failed_backup_snapshot(fixture: &Fixture) {
    zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
        .save(&AppPreferences::default())
        .unwrap();
    YamlOverrideStore::new(fixture.root.join("yaml-overrides")).unwrap();
    let controlled = ControlledConfigStore::new(fixture.root.join("controlled-config"));
    controlled.materialize(&fixture.profile).unwrap();
    let manager = zenclash_core::BackupManager::new(&fixture.root);
    let archive = fixture.root.join("retry-fixture.zip");
    manager.export_to(&archive).unwrap();
    let mut transaction = manager
        .prepare_restore(&archive)
        .unwrap()
        .activate_for_session(&fixture.core)
        .unwrap();
    let snapshot = transaction.previous_runtime_snapshot().unwrap();
    transaction.rollback_in_place().unwrap();
    let controlled = transaction.authorize_controlled_store(controlled).unwrap();
    fixture.runtime.as_ref().unwrap().block_on(async {
        let admission = fixture.core.begin_backup_restore().await.unwrap();
        assert!(
            fixture
                .core
                .restore_backup_snapshot(&controlled, &snapshot, &admission)
                .await
                .is_err()
        );
    });
    assert!(fixture.core.pending_backup_restore().is_some());
}

#[gpui_kit::test]
fn backup_retry_button_preserves_failed_snapshot_then_refreshes_after_success(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.apply_failures.store(1, Ordering::SeqCst);
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.error.is_some()
    });
    assert_eq!(
        fixture.core.pending_backup_restore(),
        Some(fixture.core.generation())
    );
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    let saved = zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"))
        .update(|preferences| preferences.appearance = zenclash_core::AppearancePreference::Dark)
        .unwrap();
    let applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = applied.clone();
    let session = fixture.core.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
            assert_eq!(event.runtime_version, session.generation());
            observed.fetch_add(1, Ordering::SeqCst);
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.click("backup-retry-runtime", cx)
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
            && applied.load(Ordering::SeqCst) == 1
            && page.notice.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.preferences, saved);
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert!(page.error.is_none());
        assert!(fixture.core.pending_backup_restore().is_none());
        assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 3);
        assert_eq!(applied.load(Ordering::SeqCst), 1);
        window.render_frame(cx);
        assert!(window.try_find("backup-retry-runtime").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn late_backup_retry_refresh_cannot_replace_a_newer_profile_or_notice(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.blocked_apply.store(true, Ordering::Release);
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |_| {
        controller.apply_requests.load(Ordering::SeqCst) == 2
    });
    controller.blocked_apply.store(false, Ordering::Release);
    let candidate = fixture.root.join("newer-profile.yaml");
    fs::write(&candidate, "mixed-port: 7999\nrules: [MATCH,DIRECT]\n").unwrap();
    fixture.runtime.as_ref().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture.core.pending_backup_restore().is_some() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        fixture
            .core
            .apply(
                &fixture.controlled,
                zenclash_core::EffectiveConfigIntent::ActivateProfile {
                    profile: candidate.clone(),
                    overrides: vec![],
                },
            )
            .await
            .unwrap();
    });
    cx.update_window(window, |_, _, cx| {
        page.update(cx, |page, cx| {
            page.synchronize_committed_profile(cx);
            page.notice = Some("newer operation notice".into());
        });
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert_eq!(page.profile_path.as_ref(), Some(&candidate));
        assert_eq!(page.notice.as_deref(), Some("newer operation notice"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_ignores_duplicate_click_and_synchronizes_after_navigation(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    controller.blocked_apply.store(true, Ordering::Release);
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |_| {
        controller.apply_requests.load(Ordering::SeqCst) == 2
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            page.read(cx)
                .mutations
                .active(busy::MutationDomain::BackupRecovery)
        );
        window.click("backup-retry-runtime", cx);
        page.update(cx, |page, cx| {
            page.switch_to(Page::Dns, cx);
            page.notice = Some("new page notice".into());
        });
    })
    .unwrap();
    controller.blocked_apply.store(false, Ordering::Release);
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert!(fixture.core.pending_backup_restore().is_none());
        assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        assert_eq!(page.notice.as_deref(), Some("new page notice"));
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_refresh_failure_keeps_the_accepted_runtime_and_reports_refresh_failure(
    cx: &mut TestAppContext,
) {
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    fs::write(
        fixture.root.join("preferences.json"),
        b"invalid persisted JSON",
    )
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("backup-retry-runtime", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.error.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        let page = page.read(cx);
        assert!(fixture.core.pending_backup_restore().is_none());
        let localized = zenclash_i18n::text("backup.errors.retry_refresh_failed");
        assert!(
            page.error
                .as_ref()
                .unwrap()
                .starts_with(localized.split("%{error}").next().unwrap())
        );
        assert_eq!(page.profile_path.as_ref(), Some(&fixture.profile));
        window.render_frame(cx);
        assert!(window.try_find("backup-retry-runtime").is_none());
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn backup_retry_is_reachable_by_tab_and_activates_once_with_enter(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let (window, page) = open(cx, &fixture, Page::Settings);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // Start contextual keyboard traversal in RuntimePage's retained focus region.
        let focus = page.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        for _ in 0..80 {
            window.press("tab", cx);
            if window.find("backup-retry-runtime").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("backup-retry-runtime").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(busy::MutationDomain::Backup) && page.notice.is_some()
    });
    assert!(fixture.core.pending_backup_restore().is_none());
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}

#[gpui_kit::test]
fn cancelled_backup_retry_waiter_still_records_the_accepted_shared_business_result(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    let controller = ControllerFixture::new(true);
    let fixture = Fixture::with_controller(controller.url.clone());
    retain_failed_backup_snapshot(&fixture);
    let commands = crate::ApplicationCommands::new(fixture.core.clone(), None);
    let record = fixture
        .profiles
        .load()
        .unwrap()
        .active_profile()
        .unwrap()
        .clone();
    let version = fixture.core.generation();
    commands
        .publish_test_outcome(
            zenclash_core::ProfileApplyOutcome::CommittedButRuntimeUnknown {
                source_version: (&record).into(),
                profile: record,
                path: fixture.profile.clone(),
                cause: zenclash_core::ProfileApplicationError::Task(
                    "fixture pending result".into(),
                ),
                runtime_version: version,
            },
        )
        .unwrap();
    controller.blocked_apply.store(true, Ordering::Release);
    let completion = commands.backup_restoration(zenclash_core::BackupManager::new(&fixture.root));
    let task = fixture
        .runtime
        .as_ref()
        .unwrap()
        .spawn(async move { completion.retry_runtime().await });
    fixture.runtime.as_ref().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while controller.apply_requests.load(Ordering::SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        controller.blocked_apply.store(false, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(5), async {
            while commands.pending_finalization().is_some()
                || fixture.core.pending_backup_restore().is_some()
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    });
    assert!(fixture.core.generation() > version);
    assert_eq!(controller.apply_requests.load(Ordering::SeqCst), 2);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn delayed_backup_completion_synchronizes_current_business_state_after_navigation(
    cx: &mut TestAppContext,
) {
    cx.foreground_executor().clone().block_test(async {
        use settings::backup::RestoreOutcome;

        let controller = ControllerFixture::new(false);
        let fixture = Fixture::with_controller(controller.url.clone());
        let preferences =
            zenclash_core::AppPreferencesStore::new(fixture.root.join("preferences.json"));
        preferences.save(&AppPreferences::default()).unwrap();
        let backup_overrides = YamlOverrideStore::new(fixture.root.join("yaml-overrides")).unwrap();
        let outcome = RestoreOutcome {
            restored: crate::application_commands::BackupRestoreReceipt::committed_for_test(
                zenclash_core::BackupManager::new(&fixture.root),
                0,
            ),
            page_data: PageSnapshotData::Empty,
        };
        let (window, page) = open(cx, &fixture, Page::Settings);
        fixture.settle(cx, &page, |page| !page.persistent_loading);

        let candidate = fixture.root.join("new-current.yaml");
        fs::write(&candidate, "mixed-port: 7991\nrules: [MATCH,DIRECT]\n").unwrap();
        let session = fixture.core.clone();
        let controlled = fixture.controlled.clone();
        let next_profile = candidate.clone();
        let applied = fixture
            .runtime
            .as_ref()
            .unwrap()
            .spawn(async move {
                session
                    .apply(
                        &controlled,
                        zenclash_core::EffectiveConfigIntent::ActivateProfile {
                            profile: next_profile,
                            overrides: Vec::new(),
                        },
                    )
                    .await
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(applied.generation, 1);
        let saved = preferences
            .update(|preferences| {
                preferences.appearance = zenclash_core::AppearancePreference::Dark;
                preferences.traffic_tray_visible = true;
            })
            .unwrap();
        let overlay = fixture.root.join("later-overlay.yaml");
        fs::write(&overlay, "mode: global\n").unwrap();
        let record = backup_overrides.import_paths([overlay]).unwrap().remove(0);
        backup_overrides.set_enabled(&record.id, false).unwrap();

        let preference_events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded_preferences = preference_events.clone();
        let _preferences_subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &PreferencesRestored, _| {
                assert_eq!(event.preferences, saved);
                assert_eq!(event.scope, PreferenceScope::Restore);
                recorded_preferences.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_paths = paths.clone();
        let _profile_subscription = cx.update(|cx| {
            cx.subscribe(&page, move |_, event: &ProfileActivated, _| {
                recorded_paths
                    .lock()
                    .unwrap()
                    .push((event.path.clone(), event.runtime_version));
            })
        });
        cx.update_window(window, |_, _, cx| {
            page.update(cx, |page, cx| {
                let token = page
                    .begin_scoped_mutation(Page::Settings, busy::MutationDomain::Backup)
                    .unwrap();
                page.switch_to(Page::Dns, cx);
                page.notice = Some("current page notice".into());
                page.apply_restore_outcome(outcome, token, cx);
                assert!(page.mutation_busy(busy::MutationDomain::Backup));
                assert!(page.begin_mutation(Page::Dns).is_none());
            });
        })
        .unwrap();
        fixture.settle(cx, &page, |page| {
            page.preferences.appearance == zenclash_core::AppearancePreference::Dark
                && page.overrides.catalog().items.len() == 1
                && !page.mutation_busy(busy::MutationDomain::Backup)
        });
        cx.update(|cx| {
            let page = page.read(cx);
            assert_eq!(page.profile_path.as_ref(), Some(&candidate));
            assert!(!page.overrides.catalog().items[0].enabled);
            assert!(page.preferences.traffic_tray_visible);
            assert_eq!(page.notice.as_deref(), Some("current page notice"));
        });
        assert_eq!(
            preference_events.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(*paths.lock().unwrap(), vec![(candidate, 1)]);
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    });
}
