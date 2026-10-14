use super::super::test_support::publish_when_available;
use super::*;
use crate::{AppPreferences, AppearancePreference};

#[test]
fn independently_opened_stores_share_revision_and_reject_a_late_snapshot() {
    let path = super::super::test_support::test_path("observed-reopen");
    let store = AppPreferencesStore::new(&path);
    let old = store.observe().unwrap();
    let independently_opened = AppPreferencesStore::new(&path);
    assert!(old.belongs_to(&independently_opened));
    let saved = independently_opened
        .update(|preferences| preferences.appearance = AppearancePreference::Dark)
        .unwrap();
    assert!(
        old.publish_if_current(|_| panic!("old preferences published"))
            .is_none()
    );
    let current = store.observe().unwrap();
    assert_eq!(current.preferences(), &saved);
    publish_when_available(&current, |preferences| {
        assert!(
            store.transaction.try_lock().is_none(),
            "publication escaped the original transaction"
        );
        assert_eq!(preferences, &saved);
    });
    std::fs::remove_file(path).unwrap();
}

#[test]
fn preference_observations_require_actual_owner_even_with_equal_values_and_revisions() {
    let first = AppPreferencesStore::new(super::super::test_support::test_path("observed-first"));
    let other = AppPreferencesStore::new(super::super::test_support::test_path("observed-other"));
    let observed = first.observe().unwrap();
    assert_eq!(observed.preferences(), &other.load().unwrap());
    assert!(!observed.belongs_to(&other));
    assert!(observed.belongs_to(&first));
}

#[test]
fn failed_preference_commit_preserves_the_last_authoritative_observation() {
    let path = super::super::test_support::test_path("observed-failed-save");
    let store = AppPreferencesStore::new(&path);
    let saved = store
        .update(|preferences| preferences.appearance = AppearancePreference::Light)
        .unwrap();
    let observed = store.observe().unwrap();
    let invalid = AppPreferences {
        traffic_retention_days: 0,
        ..saved.clone()
    };
    assert!(store.save(&invalid).is_err());
    assert_eq!(publish_when_available(&observed, Clone::clone), saved);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn complete_save_and_conditional_replace_invalidate_preceding_observations() {
    let path = super::super::test_support::test_path("observed-save-replace");
    let store = AppPreferencesStore::new(&path);
    let before_save = store.observe().unwrap();
    store.save(&AppPreferences::default()).unwrap();
    assert!(before_save.publish_if_current(|_| ()).is_none());
    let before_replace = store.observe().unwrap();
    let next = AppPreferences {
        appearance: AppearancePreference::Dark,
        ..Default::default()
    };
    store.replace(before_replace.preferences(), &next).unwrap();
    assert!(before_replace.publish_if_current(|_| ()).is_none());
    assert_eq!(store.observe().unwrap().preferences(), &next);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn preference_publication_never_waits_for_an_active_store_writer() {
    let store = AppPreferencesStore::new(super::super::test_support::test_path("observed-busy"));
    let observed = store.observe().unwrap();
    let _transaction = store.transaction.lock();
    assert!(
        observed
            .publish_if_current(|_| panic!("busy publication ran"))
            .is_none()
    );
}
