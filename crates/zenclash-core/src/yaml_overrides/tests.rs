use std::{fs, path::PathBuf, time::SystemTime};

use super::*;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zenclash-overrides-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn directory_import_persists_order_enablement_and_managed_copies() {
    let root = test_root("lifecycle");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("20-b.yaml"), "dns:\n  ipv6: true\n").unwrap();
    fs::write(source.join("10-a.yml"), "mode: direct\n").unwrap();
    fs::write(source.join("ignored.txt"), "not yaml").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();

    let imported = store.import_paths([source.clone()]).unwrap();
    assert_eq!(
        imported
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["10-a.yml", "20-b.yaml"]
    );
    fs::remove_dir_all(source).unwrap();
    let first_id = imported[0].id.clone();
    store.set_enabled(&first_id, false).unwrap();
    store.move_to(&first_id, 1).unwrap();

    let reopened = YamlOverrideStore::new(root.join("store")).unwrap();
    let catalog = reopened.load().unwrap();
    assert_eq!(catalog.items[1].id, first_id);
    assert!(!catalog.items[1].enabled);
    assert_eq!(reopened.enabled_paths(&catalog).len(), 1);
    reopened.delete(&first_id).unwrap();
    assert_eq!(reopened.load().unwrap().items.len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_directory_candidate_prevents_partial_import() {
    let root = test_root("invalid");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.yaml"), "mode: rule\n").unwrap();
    fs::write(source.join("b.yaml"), "- not\n- a\n- mapping\n").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();

    assert!(store.import_paths([source]).is_err());
    assert!(store.load().unwrap().items.is_empty());
    assert_eq!(fs::read_dir(store.files_dir()).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_manifest_path_traversal_before_file_operations() {
    let root = test_root("path-traversal");
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let outside = store.root().join("outside.yaml");
    fs::write(&outside, "mode: direct\n").unwrap();
    fs::write(
        store.manifest_path(),
        r#"{"items":[{"id":"escape","name":"escape.yaml","file_name":"../outside.yaml","enabled":false}]}"#,
    )
    .unwrap();

    assert!(matches!(store.load(), Err(YamlOverrideError::Invalid(_))));
    assert!(store.delete("escape").is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "mode: direct\n");
    fs::remove_dir_all(root).unwrap();
}
