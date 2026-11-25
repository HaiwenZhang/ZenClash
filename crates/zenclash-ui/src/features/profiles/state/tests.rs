use super::*;
use zenclash_core::{ProfileRecord, ProfileSource};

fn catalog() -> ProfileCatalog {
    ProfileCatalog {
        active: Some("profile-0".into()),
        profiles: (0..26)
            .map(|index| ProfileRecord {
                id: format!("profile-{index}"),
                name: format!("Profile {index}"),
                file_name: format!("profile-{index}.yaml"),
                source: if index % 2 == 0 {
                    ProfileSource::Local {
                        original_path: format!("profile-{index}.yaml"),
                    }
                } else {
                    ProfileSource::Remote {
                        url: "https://example.invalid/profile".into(),
                        user_agent: "clash.meta".into(),
                        options: Default::default(),
                    }
                },
                updated_at: 0,
                size_bytes: 0,
                auto_update: false,
                update_interval_minutes: 60,
                update_cron: None,
                subscription: Default::default(),
            })
            .collect(),
    }
}

#[test]
fn successful_activation_moves_inspector_but_refresh_preserves_manual_inspection() {
    let mut catalog = catalog();
    let mut view = ProfileCatalogView::default();
    view.prepare(&catalog);
    view.select("profile-1");
    view.prepare(&catalog);
    assert!(view.is_selected("profile-1"));

    catalog.active = Some("profile-2".into());
    view.prepare(&catalog);
    assert!(view.is_selected("profile-2"));
    assert_eq!(view.selected_index(), view.active_index());

    view.select("profile-3");
    view.prepare(&catalog);
    assert!(view.is_selected("profile-3"));
    assert_eq!(view.active_index(), Some(2));
}

#[test]
fn inspecting_a_subscription_does_not_activate_it_and_filters_preserve_its_identity() {
    let catalog = catalog();
    let mut view = ProfileCatalogView::default();
    view.prepare(&catalog);
    view.select("profile-21");
    view.set_filter(ProfileFilter::Remote, &catalog);
    assert_eq!(view.selected_index(), Some(21));
    assert_eq!(catalog.active.as_deref(), Some("profile-0"));
    assert!(
        view.visible_indices()
            .iter()
            .all(|&index| catalog.profiles[index].is_remote())
    );
    view.set_page(1);
    assert_eq!(view.visible_indices(), &[25]);
    view.set_filter(ProfileFilter::Local, &catalog);
    assert_eq!(view.page, 0);
    assert!(
        view.visible_indices()
            .iter()
            .all(|&index| !catalog.profiles[index].is_remote())
    );
}

#[test]
fn replaced_catalog_is_not_rendered_through_stale_indices_and_refresh_clamps_pages() {
    let catalog = catalog();
    let mut view = ProfileCatalogView::default();
    view.prepare(&catalog);
    view.set_page(2);
    assert!(view.is_current(&catalog));
    let mut replacement = catalog.clone();
    replacement.profiles.truncate(1);
    assert!(!view.is_current(&replacement));
    view.prepare(&replacement);
    assert!(view.is_current(&replacement));
    assert_eq!(view.page, 0);
    assert_eq!(view.visible_indices(), &[0]);
}

#[test]
fn active_profile_lookup_rejects_mutated_reordered_and_changed_identities() {
    let mut catalog = catalog();
    let mut view = ProfileCatalogView::default();
    view.prepare(&catalog);
    assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-0");
    // Identical content at a different heap address stays valid; in-place metadata edits invalidate.
    assert_eq!(
        view.active_profile(&catalog.clone()).unwrap().id,
        "profile-0"
    );
    catalog.profiles[0].name = "Renamed Profile 0".into();
    assert!(!view.is_current(&catalog));
    assert!(view.active_profile(&catalog).is_none());
    view.prepare(&catalog);
    assert_eq!(
        view.active_profile(&catalog).unwrap().name,
        "Renamed Profile 0"
    );
    catalog.profiles.swap(0, 1);
    assert!(view.active_profile(&catalog).is_none());
    view.prepare(&catalog);
    assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-0");
    catalog.active = Some("profile-1".into());
    assert!(view.active_profile(&catalog).is_none());
    view.prepare(&catalog);
    assert_eq!(view.active_profile(&catalog).unwrap().id, "profile-1");
    catalog.active = Some("missing".into());
    view.prepare(&catalog);
    assert!(view.active_profile(&catalog).is_none());
    catalog.active = None;
    view.prepare(&catalog);
    assert!(view.active_profile(&catalog).is_none());
}
