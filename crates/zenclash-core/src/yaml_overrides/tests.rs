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
fn directory_import_rejects_excess_files_before_parsing_any_payload() {
    let root = test_root("candidate-count");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    for index in 0..=MAX_OVERRIDE_COUNT {
        fs::write(source.join(format!("{index}.yaml")), "[invalid mapping").unwrap();
    }
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let error = store.import_paths([source]).unwrap_err();
    assert!(error.to_string().contains("单次导入"), "{error}");
    assert!(store.load().unwrap().items.is_empty());
    assert_eq!(fs::read_dir(store.files_dir()).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_disabled_view_cannot_delete_a_now_enabled_record() {
    let root = test_root("enabled-delete");
    fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    fs::write(&source, "mode: direct\n").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let record = store.import_paths([source]).unwrap().remove(0);
    assert!(store.delete_disabled(&record.id).is_err());
    assert_eq!(store.load().unwrap().items, vec![record]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn import_compensation_retains_a_record_changed_by_another_writer() {
    let root = test_root("changed-import");
    fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    fs::write(&source, "mode: direct\n").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let record = store.import_paths([source]).unwrap().remove(0);
    store.set_enabled(&record.id, false).unwrap();
    assert!(store.discard_imported(&record).is_err());
    assert!(!store.load().unwrap().items[0].enabled);
    assert!(store.managed_path(&record).is_file());
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

#[test]
fn malformed_manifest_is_quarantined_and_managed_files_are_retained() {
    let root = test_root("quarantine-manifest");
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let managed = store.files_dir().join("kept.yaml");
    fs::write(&managed, "mode: direct\n").unwrap();
    let invalid = b"{not-json";
    fs::write(store.manifest_path(), invalid).unwrap();

    assert!(matches!(store.load(), Err(YamlOverrideError::Manifest(_))));
    let quarantine = store.quarantine_invalid_manifest().unwrap().unwrap();

    assert_eq!(fs::read(quarantine).unwrap(), invalid);
    assert_eq!(fs::read_to_string(managed).unwrap(), "mode: direct\n");
    assert!(store.load().unwrap().items.is_empty());
    assert!(store.quarantine_invalid_manifest().unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_referencing_missing_file_is_quarantined_and_store_recovers() {
    let root = test_root("missing-file-quarantine");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.yaml"), "mode: direct\n").unwrap();
    fs::write(source.join("b.yaml"), "mode: rule\n").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let imported = store.import_paths([source]).unwrap();
    fs::remove_file(store.managed_path(&imported[0])).unwrap();

    assert!(matches!(store.load(), Err(YamlOverrideError::NotFound(_))));
    let quarantine = store.quarantine_invalid_manifest().unwrap().unwrap();
    assert!(quarantine.is_file());
    assert!(store.managed_path(&imported[1]).is_file());
    assert!(store.load().unwrap().items.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn delete_keeps_override_file_intact_when_manifest_save_fails() {
    use std::os::unix::fs::PermissionsExt as _;

    // Root bypasses directory write permissions; skip when running as uid 0.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = test_root("delete-manifest-first");
    let source = root.join("source.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "mode: direct\n").unwrap();
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    let record = store.import_paths([source]).unwrap().remove(0);
    let managed = store.managed_path(&record);

    // Make only the store root read-only so writing the new manifest temp file
    // fails while files/ remains writable. Because the manifest must be saved
    // before removing the YAML file, the YAML file must never be deleted.
    let original_perms = fs::metadata(store.root()).unwrap().permissions();
    let mut readonly = original_perms.clone();
    readonly.set_mode(0o500);
    fs::set_permissions(store.root(), readonly).unwrap();

    let result = store.delete(&record.id);
    fs::set_permissions(store.root(), original_perms).unwrap();

    assert!(result.is_err());
    assert!(
        managed.is_file(),
        "override file must remain on disk when manifest persistence fails"
    );
    assert_eq!(store.load().unwrap().items, vec![record]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_rejects_case_insensitive_duplicate_file_names() {
    let root = test_root("case-insensitive-dup");
    let store = YamlOverrideStore::new(root.join("store")).unwrap();
    fs::write(store.files_dir().join("alpha.yaml"), "mode: direct\n").unwrap();
    let _ = fs::write(store.files_dir().join("ALPHA.yaml"), "mode: rule\n");
    fs::write(
        store.manifest_path(),
        r#"{"items":[{"id":"first","name":"alpha.yaml","file_name":"alpha.yaml","enabled":true},{"id":"second","name":"ALPHA.yaml","file_name":"ALPHA.yaml","enabled":true}]}"#,
    )
    .unwrap();

    assert!(matches!(store.load(), Err(YamlOverrideError::Invalid(_))));
    fs::remove_dir_all(root).unwrap();
}
