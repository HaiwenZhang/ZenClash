use super::*;
use crate::pages::runtime::{
    PreferencesRestored,
    busy::MutationDomain,
    test_support::{RuntimePageFixture as Fixture, open_runtime_page_window as open},
};
use gpui_kit::{AppContext, TestAppContext};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use zenclash_core::{AppPreferencesStore, AppearancePreference};

#[gpui_kit::test]
fn a_restore_snapshot_read_before_a_new_save_cannot_publish_old_preferences(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let services = fixture.services();
    let root = services.controlled_config_store.root().parent().unwrap();
    let store = AppPreferencesStore::new(root.join("preferences.json"));
    store
        .update(|preferences| preferences.appearance = AppearancePreference::Light)
        .unwrap();
    let restored = crate::application_commands::BackupRestoreReceipt::committed_for_test(
        BackupManager::new(root),
        0,
    )
    .refresh_snapshot()
    .unwrap();
    let latest = store
        .update(|preferences| preferences.appearance = AppearancePreference::Dark)
        .unwrap();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let events = Arc::new(AtomicUsize::new(0));
    let observed_events = events.clone();
    let expected = latest.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&page, move |_, event: &PreferencesRestored, _| {
            assert_eq!(event.preferences, expected);
            assert!(event.observation().is_some());
            observed_events.fetch_add(1, Ordering::SeqCst);
        })
    });
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.preferences_restored_from_app(latest, Default::default(), cx);
            let token = page
                .begin_scoped_mutation(Page::Settings, MutationDomain::Backup)
                .unwrap();
            page.finish_restore_outcome(
                RestoreOutcome {
                    restored,
                    page_data: crate::pages::runtime::PageSnapshotData::Empty,
                },
                token,
                cx,
            );
            assert_eq!(page.preferences.appearance, AppearancePreference::Dark);
            assert!(
                page.mutation_busy(MutationDomain::Backup),
                "expired snapshot finished mutation before current preferences were adopted"
            );
            assert!(page.begin_mutation(Page::Settings).is_none());
        })
    });
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).preferences.appearance,
            AppearancePreference::Dark
        );
        assert_eq!(store.load().unwrap().appearance, AppearancePreference::Dark);
        assert_eq!(events.load(Ordering::SeqCst), 1);
        window.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn failed_current_preferences_read_finishes_restore_mutation_without_publishing_old_values(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let services = fixture.services();
    let root = services.controlled_config_store.root().parent().unwrap();
    let store = AppPreferencesStore::new(root.join("preferences.json"));
    store
        .save(&zenclash_core::AppPreferences::default())
        .unwrap();
    let restored = crate::application_commands::BackupRestoreReceipt::committed_for_test(
        BackupManager::new(root),
        0,
    )
    .refresh_snapshot()
    .unwrap();
    let (window, page) = open(cx, &fixture, Page::Settings);
    fixture.settle(cx, &page, |page| !page.persistent_loading);
    let saved = store
        .update(|preferences| preferences.appearance = AppearancePreference::Dark)
        .unwrap();
    std::fs::remove_file(store.path()).unwrap();
    std::fs::create_dir(store.path()).unwrap();
    cx.update(|cx| {
        page.update(cx, |page, cx| {
            page.preferences_restored_from_app(saved, Default::default(), cx);
            let token = page
                .begin_scoped_mutation(Page::Settings, MutationDomain::Backup)
                .unwrap();
            page.finish_restore_outcome(
                RestoreOutcome {
                    restored,
                    page_data: crate::pages::runtime::PageSnapshotData::Empty,
                },
                token,
                cx,
            );
        })
    });
    fixture.settle(cx, &page, |page| {
        !page.mutation_busy(MutationDomain::Backup)
    });
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            page.read(cx).preferences.appearance,
            AppearancePreference::Dark
        );
        assert!(page.read(cx).error.is_some());
        page.update(cx, |page, _| {
            let next = page
                .mutations
                .begin(MutationDomain::Core)
                .expect("restore kept the mutation reserved after failed read");
            page.mutations.finish(next);
        });
        window.remove_window();
    })
    .unwrap();
}
