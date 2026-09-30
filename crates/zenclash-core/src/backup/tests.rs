use std::{
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::{
    AppPreferences, AppearancePreference, ControlledConfigStore, ProfileStore, YamlOverrideStore,
    profiles::atomic_write,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PROFILE: &str = "mixed-port: 7890\nproxies: []\nproxy-groups: []\nrules: []\n";

#[test]
fn export_rejects_an_index_whose_pretty_encoding_exceeds_the_store_capacity() {
    let root = test_root("catalog-export-capacity");
    let source = root.join("source");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    let store = ProfileStore::new(source.join("profiles")).unwrap();
    let mut catalog = store.load().unwrap();
    let limit = 4 * 1024 * 1024;
    catalog.profiles[0].name = "x".repeat(limit);
    let overhead = serde_json::to_vec(&catalog).unwrap().len() - limit;
    catalog.profiles[0].name.truncate(limit - overhead - 1);
    fs::write(
        source.join(PROFILE_INDEX_PATH),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    assert!(store.load().is_ok());
    let destination = root.join("backup.zip");
    assert!(matches!(
        BackupManager::new(&source).export_to(&destination),
        Err(BackupError::Profiles(
            ProfileStoreError::IndexTooLarge { .. }
        ))
    ));
    assert!(!destination.exists());
    assert!(store.load().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_an_oversized_valid_index_without_changing_live_data() {
    use sha2::{Digest, Sha256};

    let root = test_root("catalog-restore-capacity");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 7890);
    create_snapshot(&target, AppearancePreference::Dark, true, 7891);
    let before = read_authoritative_snapshot(&target);
    let archive = root.join("backup.zip");
    BackupManager::new(&source).export_to(&archive).unwrap();
    let mut catalog = ProfileStore::new(source.join("profiles"))
        .unwrap()
        .load()
        .unwrap();
    catalog.profiles[0].name = "x".repeat(4 * 1024 * 1024);
    let index = serde_json::to_vec(&catalog).unwrap();
    let oversized = root.join("oversized.zip");
    rewrite_archive(&archive, &oversized, |name, bytes| {
        if name == PROFILE_INDEX_PATH {
            *bytes = index.clone();
        }
        if name == MANIFEST_PATH {
            let mut manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            let entry = manifest["files"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["path"] == PROFILE_INDEX_PATH)
                .unwrap();
            entry["size"] = serde_json::json!(index.len());
            entry["sha256"] = serde_json::json!(format!("{:x}", Sha256::digest(&index)));
            *bytes = serde_json::to_vec(&manifest).unwrap();
        }
    });
    assert!(matches!(
        BackupManager::new(&target).prepare_restore(&oversized),
        Err(BackupError::Profiles(
            ProfileStoreError::IndexTooLarge { .. }
        ))
    ));
    assert_eq!(read_authoritative_snapshot(&target), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_restore_is_complete_reversible_and_excludes_generated_cache() {
    let root = test_root("roundtrip");
    let source = root.join("source");
    let target = root.join("target");
    let archive = root.join("backup.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    fs::write(
        source.join("controlled-config/effective.yaml"),
        "mixed-port: 6553\n",
    )
    .unwrap();

    let summary = BackupManager::new(&source).export_to(&archive).unwrap();

    assert_eq!(summary.file_count, 6);
    assert!(summary.payload_bytes > 0);
    assert!(
        !archive_names(&archive)
            .iter()
            .any(|name| name.ends_with("effective.yaml"))
    );
    let original_target = read_authoritative_snapshot(&target);
    let prepared = BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original_target);

    let transaction = prepared.activate().unwrap();
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        AppearancePreference::Light
    );
    assert!(
        ControlledConfigStore::new(target.join("controlled-config"))
            .load_json()
            .unwrap()["mixed-port"]
            .as_u64()
            .is_some_and(|port| port == 17_890)
    );
    let restored_overrides = YamlOverrideStore::new(target.join("yaml-overrides"))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(restored_overrides.items.len(), 1);
    transaction.rollback().unwrap();
    assert_eq!(read_authoritative_snapshot(&target), original_target);

    BackupManager::new(&target)
        .prepare_restore(&archive)
        .unwrap()
        .activate()
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        AppPreferencesStore::new(target.join("preferences.json"))
            .load()
            .unwrap()
            .appearance,
        AppearancePreference::Light
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_checksum_mismatch_before_touching_live_data() {
    let root = test_root("checksum");
    let source = root.join("source");
    let target = root.join("target");
    let archive = root.join("backup.zip");
    let tampered = root.join("tampered.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    BackupManager::new(&source).export_to(&archive).unwrap();
    rewrite_archive(&archive, &tampered, |name, bytes| {
        if name == PREFERENCES_PATH {
            bytes.push(b' ');
        }
    });
    let previous = read_authoritative_snapshot(&target);

    let error = BackupManager::new(&target)
        .prepare_restore(&tampered)
        .unwrap_err();

    assert!(error.to_string().contains("SHA-256"));
    assert_eq!(read_authoritative_snapshot(&target), previous);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_rejects_zip_slip_paths() {
    let root = test_root("zip-slip");
    fs::create_dir_all(&root).unwrap();
    let archive = root.join("unsafe.zip");
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    writer
        .start_file("../preferences.json", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"{}").unwrap();
    fs::write(&archive, writer.finish().unwrap().into_inner()).unwrap();

    let error = BackupManager::new(root.join("live"))
        .prepare_restore(&archive)
        .unwrap_err();

    assert!(error.to_string().contains("不安全"));
    assert!(!root.join("preferences.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_profile_catalog_is_still_a_valid_backup_snapshot() {
    let root = test_root("empty-profiles");
    let source = root.join("source");
    let archive = root.join("backup.zip");
    fs::create_dir_all(&source).unwrap();

    let summary = BackupManager::new(&source).export_to(&archive).unwrap();
    let prepared = BackupManager::new(root.join("target"))
        .prepare_restore(&archive)
        .unwrap();

    assert_eq!(summary.file_count, 4);
    assert_eq!(prepared.file_count(), 4);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restores_legacy_v1_snapshot_with_an_empty_override_catalog() {
    let root = test_root("legacy-v1");
    let source = root.join("source");
    let archive = root.join("backup-v2.zip");
    let legacy = root.join("backup-v1.zip");
    create_snapshot(&source, AppearancePreference::Light, false, 17_890);
    BackupManager::new(&source).export_to(&archive).unwrap();
    make_legacy_v1_archive(&archive, &legacy);

    BackupManager::new(root.join("target"))
        .prepare_restore(&legacy)
        .unwrap()
        .activate()
        .unwrap()
        .commit()
        .unwrap();

    let overrides = YamlOverrideStore::new(root.join("target/yaml-overrides"))
        .unwrap()
        .load()
        .unwrap();
    assert!(overrides.items.is_empty());
    fs::remove_dir_all(root).unwrap();
}

fn create_snapshot(root: &Path, appearance: AppearancePreference, tray: bool, port: u16) {
    fs::create_dir_all(root).unwrap();
    AppPreferencesStore::new(root.join("preferences.json"))
        .save(&AppPreferences {
            appearance,
            traffic_tray_visible: tray,
            ..AppPreferences::default()
        })
        .unwrap();
    atomic_write(
        &root.join("controlled-config/override.yaml"),
        format!("mixed-port: {port}\n").as_bytes(),
    )
    .unwrap();
    let source = root.join("import.yaml");
    fs::write(&source, PROFILE).unwrap();
    let profiles = ProfileStore::new(root.join("profiles")).unwrap();
    let record = profiles.import_local(source).unwrap();
    profiles.activate(&record.id).unwrap();
    let override_source = root.join("managed-override.yaml");
    fs::write(&override_source, format!("mixed-port: {}\n", port + 100)).unwrap();
    YamlOverrideStore::new(root.join("yaml-overrides"))
        .unwrap()
        .import_paths([override_source])
        .unwrap();
}

fn read_authoritative_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut snapshot = Vec::new();
    for relative in [
        "preferences.json",
        "controlled-config/override.yaml",
        "profiles/profiles.json",
        "yaml-overrides/overrides.json",
    ] {
        snapshot.push((relative.into(), fs::read(root.join(relative)).unwrap()));
    }
    let mut profiles = fs::read_dir(root.join("profiles/files"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    profiles.sort();
    for path in profiles {
        snapshot.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            fs::read(path).unwrap(),
        ));
    }
    let mut overrides = fs::read_dir(root.join("yaml-overrides/files"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    overrides.sort();
    for path in overrides {
        snapshot.push((
            format!("override:{}", path.file_name().unwrap().to_string_lossy()),
            fs::read(path).unwrap(),
        ));
    }
    snapshot
}

fn archive_names(path: &Path) -> Vec<String> {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|index| zip.by_index(index).unwrap().name().to_owned())
        .collect()
}

fn rewrite_archive(source: &Path, destination: &Path, mutate: impl Fn(&str, &mut Vec<u8>)) {
    let mut reader = ZipArchive::new(fs::File::open(source).unwrap()).unwrap();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..reader.len() {
        let mut entry = reader.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        mutate(&name, &mut bytes);
        writer
            .start_file(&name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    fs::write(destination, writer.finish().unwrap().into_inner()).unwrap();
}

fn make_legacy_v1_archive(source: &Path, destination: &Path) {
    let mut reader = ZipArchive::new(fs::File::open(source).unwrap()).unwrap();
    let mut entries = Vec::new();
    for index in 0..reader.len() {
        let mut entry = reader.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if !name.starts_with("yaml-overrides/") {
            entries.push((name, bytes));
        }
    }
    let manifest = entries
        .iter_mut()
        .find(|(name, _)| name == MANIFEST_PATH)
        .unwrap();
    let mut decoded: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
    decoded["format_version"] = serde_json::json!(1);
    decoded["files"].as_array_mut().unwrap().retain(|file| {
        !file["path"]
            .as_str()
            .unwrap()
            .starts_with("yaml-overrides/")
    });
    manifest.1 = serde_json::to_vec_pretty(&decoded).unwrap();

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    fs::write(destination, writer.finish().unwrap().into_inner()).unwrap();
}

fn test_root(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "zenclash-backup-{name}-{}-{sequence}",
        std::process::id()
    ))
}
