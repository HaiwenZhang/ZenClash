use std::fs;

use anyhow::Result;
use zenclash_core::{
    mihomo::SystemProxyMode,
    runtime::ControlledConfigStore,
    storage::{
        AppPreferencesError, AppPreferencesStore, AppearancePreference, LanguagePreference,
        ProfileStore, TrafficDimension, TrafficHistoryEntry, TrafficHistoryQuery,
        TrafficHistoryStore, YamlOverrideStore,
    },
    workflows::BackupManager,
};

use crate::TempDir;

#[test]
fn profile_override_and_preferences_stores_persist_across_reopen() -> Result<()> {
    let temp = TempDir::new("stores-crud")?;
    let sample_profile = temp.path().join("sample-office.yaml");
    let initial_yaml = "mixed-port: 7890\nmode: rule\nrules:\n  - MATCH,DIRECT\n";
    fs::write(&sample_profile, initial_yaml)?;

    // ProfileStore CRUD + reopen persistence.
    let profiles_root = temp.path().join("profiles");
    let profile_store = ProfileStore::new(&profiles_root)?;
    let imported = profile_store.import_local(&sample_profile)?;
    assert_eq!(imported.name, "sample-office");
    let active = profile_store.activate(&imported.id)?;
    assert_eq!(active, profile_store.profile_path(&imported));
    assert_eq!(
        profile_store.active_path()?,
        Some(profile_store.profile_path(&imported))
    );

    let edited_yaml = "mixed-port: 7891\nmode: direct\nrules:\n  - MATCH,DIRECT\n";
    let _update = profile_store.replace_payload(&imported.id, initial_yaml, edited_yaml)?;

    let reopened_profiles = ProfileStore::new(&profiles_root)?;
    let catalog = reopened_profiles.load()?;
    assert_eq!(catalog.profiles.len(), 1);
    assert_eq!(
        catalog.active_profile().map(|record| record.id.as_str()),
        Some(imported.id.as_str())
    );
    assert_eq!(
        fs::read_to_string(reopened_profiles.profile_path(&imported))?,
        edited_yaml
    );

    // YamlOverrideStore CRUD, reorder, disable + reopen persistence.
    let override_src = temp.path().join("override-sources");
    fs::create_dir_all(&override_src)?;
    fs::write(override_src.join("01-first.yaml"), "mode: global\n")?;
    fs::write(override_src.join("02-second.yaml"), "log-level: debug\n")?;

    let overrides_root = temp.path().join("yaml-overrides");
    let override_store = YamlOverrideStore::new(&overrides_root)?;
    let imported_overrides = override_store.import_paths([override_src])?;
    assert_eq!(imported_overrides.len(), 2);

    let before = override_store.load()?;
    let mut updated = before.clone();
    updated.items.swap(0, 1);
    updated.items[1].enabled = false;
    override_store.replace_catalog(&before, &updated)?;

    let reopened_overrides = YamlOverrideStore::new(&overrides_root)?;
    let loaded_overrides = reopened_overrides.load_validated()?;
    assert_eq!(loaded_overrides.items.len(), 2);
    assert_eq!(loaded_overrides.items[0].id, imported_overrides[1].id);
    assert!(!loaded_overrides.items[1].enabled);
    assert_eq!(reopened_overrides.enabled_paths(&loaded_overrides).len(), 1);

    // AppPreferencesStore update, optimistic concurrency check + reopen persistence.
    let prefs_path = temp.path().join("preferences.json");
    let prefs_store = AppPreferencesStore::new(&prefs_path);
    let initial_prefs = prefs_store.load()?;
    let saved = prefs_store.update(|prefs| {
        prefs.appearance = AppearancePreference::Dark;
        prefs.language = LanguagePreference::En;
        prefs.traffic_retention_days = 14;
        prefs.system_proxy_mode = SystemProxyMode::Pac;
        prefs.system_proxy_host = "127.0.0.1".to_owned();
    })?;

    // Stale optimistic replacement must fail with ConcurrentModification.
    let stale_result = prefs_store.replace(&initial_prefs, &saved);
    assert!(matches!(
        stale_result,
        Err(AppPreferencesError::ConcurrentModification)
    ));

    let reopened_prefs = AppPreferencesStore::new(&prefs_path).load()?;
    assert_eq!(reopened_prefs, saved);
    assert_eq!(reopened_prefs.appearance, AppearancePreference::Dark);
    assert_eq!(reopened_prefs.language, LanguagePreference::En);
    assert_eq!(reopened_prefs.traffic_retention_days, 14);
    assert_eq!(reopened_prefs.system_proxy_mode, SystemProxyMode::Pac);

    Ok(())
}

#[test]
fn traffic_history_store_records_queries_and_enforces_retention() -> Result<()> {
    let temp = TempDir::new("traffic-history")?;
    let db_path = temp.path().join("traffic-history.sqlite3");
    let store = TrafficHistoryStore::new(&db_path);

    let entries = vec![
        // Expired before cutoff (should be pruned atomically).
        TrafficHistoryEntry {
            timestamp_ms: 1_000,
            source_ip: "127.0.0.1".into(),
            host: "expired.example".into(),
            outbound: "DIRECT".into(),
            process: "curl".into(),
            upload: 500,
            download: 1_500,
        },
        // Retained entries inside query window.
        TrafficHistoryEntry {
            timestamp_ms: 10_000,
            source_ip: "127.0.0.1".into(),
            host: "api.example.com".into(),
            outbound: "ProxyA".into(),
            process: "zenclash".into(),
            upload: 1_024,
            download: 4_096,
        },
        TrafficHistoryEntry {
            timestamp_ms: 15_000,
            source_ip: "127.0.0.1".into(),
            host: "api.example.com".into(),
            outbound: "ProxyA".into(),
            process: "zenclash".into(),
            upload: 2_048,
            download: 8_192,
        },
        TrafficHistoryEntry {
            timestamp_ms: 18_000,
            source_ip: "192.168.1.10".into(),
            host: "cdn.example.net".into(),
            outbound: "DIRECT".into(),
            process: "browser".into(),
            upload: 256,
            download: 2_048,
        },
    ];
    store.insert_and_cleanup(&entries, 5_000)?;

    let query = TrafficHistoryQuery {
        dimension: TrafficDimension::Host,
        start_ms: 0,
        end_ms: 20_000,
        bucket_ms: 5_000,
    };
    let overview = store.overview(&query)?;
    assert_eq!(overview.totals.samples, 3);
    assert_eq!(overview.totals.upload, 1_024 + 2_048 + 256);
    assert_eq!(overview.totals.download, 4_096 + 8_192 + 2_048);
    assert_eq!(overview.rankings.len(), 2);
    assert_eq!(overview.rankings[0].label, "api.example.com");
    assert_eq!(overview.rankings[0].total, 15_360);
    assert_eq!(overview.rankings[1].label, "cdn.example.net");

    let details = store.details(&query, "api.example.com")?;
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].label, "127.0.0.1");

    let proxy_stats = store.proxy_stats(&query, "api.example.com", "127.0.0.1")?;
    assert_eq!(proxy_stats.len(), 1);
    assert_eq!(proxy_stats[0].label, "ProxyA");

    // Reopen fresh handle on same SQLite path to verify durability, then clear.
    store.close_connection();
    let reopened = TrafficHistoryStore::new(&db_path);
    assert_eq!(reopened.overview(&query)?.totals.samples, 3);
    reopened.clear()?;
    assert_eq!(reopened.overview(&query)?.totals.samples, 0);

    Ok(())
}

#[test]
fn backup_manager_round_trips_export_rollback_and_committed_restore() -> Result<()> {
    let temp = TempDir::new("backup-roundtrip")?;
    let source_root = temp.path().join("source-data");
    let dest_root = temp.path().join("dest-data");
    let archive_path = temp.path().join("zenclash-backup.zip");
    fs::create_dir_all(&source_root)?;
    fs::create_dir_all(&dest_root)?;

    // Seed source authoritative data across preferences, profiles, overrides, and controlled config.
    let src_prefs = AppPreferencesStore::new(source_root.join("preferences.json"));
    src_prefs.update(|prefs| {
        prefs.appearance = AppearancePreference::Light;
        prefs.language = LanguagePreference::En;
        prefs.traffic_retention_days = 45;
    })?;

    let src_profile_file = temp.path().join("prod.yaml");
    fs::write(
        &src_profile_file,
        "mixed-port: 7890\nmode: rule\nrules:\n  - MATCH,DIRECT\n",
    )?;
    let src_profiles = ProfileStore::new(source_root.join("profiles"))?;
    let src_record = src_profiles.import_local(&src_profile_file)?;
    src_profiles.activate(&src_record.id)?;

    let src_override_file = temp.path().join("tuning.yaml");
    fs::write(&src_override_file, "log-level: warning\n")?;
    let src_overrides = YamlOverrideStore::new(source_root.join("yaml-overrides"))?;
    src_overrides.import_paths([src_override_file])?;

    let src_controlled = ControlledConfigStore::new(source_root.join("controlled-config"));
    let controlled_update = src_controlled.prepare_json_update(
        src_profiles.profile_path(&src_record),
        &serde_json::json!({"mode": "global", "allow-lan": true}),
    )?;
    src_controlled.commit(&controlled_update)?;

    // Export source root to a versioned ZIP archive.
    let source_manager = BackupManager::new(&source_root);
    let summary = source_manager.export_to(&archive_path)?;
    assert_eq!(summary.path, archive_path);
    assert!(summary.file_count >= 4);
    assert!(summary.payload_bytes > 0);
    assert!(archive_path.is_file());

    // Seed distinct initial data in destination root so we can verify rollback vs commit.
    let dst_prefs = AppPreferencesStore::new(dest_root.join("preferences.json"));
    dst_prefs.update(|prefs| {
        prefs.appearance = AppearancePreference::Dark;
        prefs.traffic_retention_days = 7;
    })?;

    let dest_manager = BackupManager::new(&dest_root);

    // 1. Dropping an uncommitted restore transaction must roll back to previous destination data.
    let prepared_rollback = dest_manager.prepare_restore(&archive_path)?;
    assert_eq!(prepared_rollback.file_count(), summary.file_count);
    assert_eq!(prepared_rollback.payload_bytes(), summary.payload_bytes);
    let uncommitted = prepared_rollback.activate()?;
    drop(uncommitted);

    let after_rollback = dest_manager.read_snapshot()?;
    assert_eq!(
        after_rollback.preferences.appearance,
        AppearancePreference::Dark
    );
    assert_eq!(after_rollback.preferences.traffic_retention_days, 7);
    assert!(after_rollback.profiles.profiles.is_empty());

    // 2. Committing the restore transaction atomically replaces all authoritative stores.
    let prepared_commit = dest_manager.prepare_restore(&archive_path)?;
    prepared_commit.activate()?.commit()?;

    let restored = dest_manager.read_snapshot()?;
    assert_eq!(restored.preferences.appearance, AppearancePreference::Light);
    assert_eq!(restored.preferences.language, LanguagePreference::En);
    assert_eq!(restored.preferences.traffic_retention_days, 45);
    assert_eq!(restored.profiles.profiles.len(), 1);
    assert_eq!(restored.profiles.profiles[0].name, "prod");
    assert_eq!(restored.overrides.items.len(), 1);
    assert_eq!(restored.controlled_config["mode"], "global");
    assert_eq!(restored.controlled_config["allow-lan"], true);

    Ok(())
}
