use zenclash_core::{ControlledConfigStore, CoreKind};

use super::*;

#[test]
fn scanner_returns_the_profile_override_chain_and_digest_it_checked() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-source-scan-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let profile = root.join("profile.yaml");
    let override_path = root.join("override.yaml");
    std::fs::write(&profile, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
    std::fs::write(&override_path, "mode: direct\n").unwrap();
    let store = ControlledConfigStore::new(root.join("controlled"));
    store
        .materialize_with_overrides_for_core(
            &profile,
            std::slice::from_ref(&override_path),
            CoreKind::Mihomo,
        )
        .unwrap();
    let mut scanner = SourceScanner::default();
    assert!(
        scanner
            .scan_with_override_paths(
                &store,
                CoreKind::Mihomo,
                profile.clone(),
                vec![override_path.clone()]
            )
            .unwrap()
            .is_none()
    );
    std::fs::write(&override_path, "mode: global\n# different metadata size\n").unwrap();
    let changed = scanner
        .scan_with_override_paths(
            &store,
            CoreKind::Mihomo,
            profile.clone(),
            vec![override_path.clone()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(changed.source_profile, profile);
    assert_eq!(changed.override_paths, vec![override_path.clone()]);
    let retained = scanner
        .scan_with_override_paths(
            &store,
            CoreKind::Mihomo,
            profile.clone(),
            vec![override_path.clone()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(retained.digest, changed.digest);
    store
        .materialize_with_overrides_for_core(&profile, &[override_path], CoreKind::Mihomo)
        .unwrap();
    assert!(
        scanner
            .scan_with_override_paths(&store, CoreKind::Mihomo, profile, changed.override_paths)
            .unwrap()
            .is_none()
    );
    assert!(
        scanner
            .scan(&store, CoreKind::Mihomo, None)
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scanner_uses_injected_override_store_without_discovery() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-source-scan-injected-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let profile = root.join("profile.yaml");
    let override_src = root.join("override.yaml");
    std::fs::write(&profile, "mode: rule\nrules: ['MATCH,DIRECT']\n").unwrap();
    std::fs::write(&override_src, "mode: direct\n").unwrap();
    let overrides = YamlOverrideStore::new(root.join("yaml-overrides")).unwrap();
    let imported = overrides.import_paths([override_src]).unwrap();
    assert_eq!(imported.len(), 1);
    let enabled_paths = overrides.load_enabled_paths().unwrap();

    let controlled = ControlledConfigStore::new(root.join("controlled"));
    controlled
        .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
        .unwrap();

    let mut without_store = SourceScanner::new(None);
    assert!(
        without_store
            .scan(&controlled, CoreKind::Mihomo, Some(profile.clone()))
            .unwrap()
            .is_none()
    );

    let mut with_store = SourceScanner::new(Some(overrides));
    let pending = with_store
        .scan(&controlled, CoreKind::Mihomo, Some(profile.clone()))
        .unwrap()
        .expect("injected override store must produce a pending revision");
    assert_eq!(pending.source_profile, profile);
    assert_eq!(pending.override_paths, enabled_paths);
    std::fs::remove_dir_all(root).unwrap();
}
