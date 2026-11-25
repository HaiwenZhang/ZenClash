use super::test_support::test_path;
use super::*;

#[test]
fn removed_tray_display_preferences_preserve_other_settings_and_are_not_saved_again() {
    for display in ["traffic", "icon"] {
        let path = test_path(&format!("removed-tray-display-{display}"));
        let store = AppPreferencesStore::new(&path);
        let mut old = serde_json::to_value(AppPreferences {
            appearance: AppearancePreference::Dark,
            language: LanguagePreference::En,
            traffic_tray_visible: false,
            ..Default::default()
        })
        .unwrap();
        old["tray_display"] = display.into();
        let bytes = serde_json::to_vec(&old).unwrap();
        fs::write(&path, &bytes).unwrap();

        let preferences = store.load().unwrap();
        assert_eq!(preferences.appearance, AppearancePreference::Dark);
        assert_eq!(preferences.language, LanguagePreference::En);
        assert!(!preferences.traffic_tray_visible);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store.save(&preferences).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved.get("tray_display").is_none());
        assert_eq!(store.load().unwrap(), preferences);
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn removed_tray_display_migration_still_rejects_invalid_or_duplicate_settings() {
    let path = test_path("invalid-removed-tray-display");
    let store = AppPreferencesStore::new(&path);
    for bytes in [
        r#"{"tray_display":"unsupported"}"#,
        r#"{"tray_display":"traffic","tray_display":"icon"}"#,
        r#"{"tray_display":"traffic","language":"unsupported"}"#,
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn removed_runtime_preferences_migrate_without_losing_other_settings() {
    let path = test_path("removed-runtime-migration");
    let store = AppPreferencesStore::new(&path);
    let mut old = serde_json::to_value(AppPreferences {
        appearance: AppearancePreference::Dark,
        language: LanguagePreference::En,
        traffic_retention_days: 30,
        ..Default::default()
    })
    .unwrap();
    old["core_kind"] = "meow".into();
    old["core_binaries"] = serde_json::json!({"mihomo": "/kept/mihomo", "meow": "/retired/engine"});
    old["last_known_good_core"] = "meow".into();
    old["last_known_good_binary"] = "/retired/engine".into();
    let bytes = serde_json::to_vec(&old).unwrap();
    fs::write(&path, &bytes).unwrap();

    let preferences = store.load().unwrap();

    assert_eq!(preferences.core_kind, CoreKind::Mihomo);
    assert_eq!(
        preferences.core_binaries.mihomo.as_deref(),
        Some(Path::new("/kept/mihomo"))
    );
    assert!(preferences.last_known_good_core.is_none());
    assert!(preferences.last_known_good_binary.is_none());
    assert_eq!(preferences.appearance, AppearancePreference::Dark);
    assert_eq!(preferences.language, LanguagePreference::En);
    assert_eq!(preferences.traffic_retention_days, 30);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    store.save(&preferences).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(saved["core_binaries"].get("meow").is_none());
    assert_eq!(store.load().unwrap(), preferences);
    fs::remove_file(path).unwrap();
}

#[test]
fn retired_binary_field_keeps_the_valid_mihomo_recovery_identity() {
    let path = test_path("retired-binary-field");
    let store = AppPreferencesStore::new(&path);
    let expected = AppPreferences {
        core_binaries: CoreBinaryPreferences {
            mihomo: Some("/current/mihomo".into()),
        },
        last_known_good_core: Some(CoreKind::Mihomo),
        last_known_good_binary: Some("/known/mihomo".into()),
        ..Default::default()
    };
    let mut old = serde_json::to_value(&expected).unwrap();
    old["core_binaries"]["meow"] = serde_json::Value::Null;
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

    assert_eq!(store.load().unwrap(), expected);
    fs::remove_file(path).unwrap();
}

#[test]
fn removed_runtime_migration_does_not_accept_other_invalid_preferences() {
    let path = test_path("invalid-retired-preferences");
    let store = AppPreferencesStore::new(&path);
    for invalid in [
        serde_json::json!({"core_kind": "unsupported", "core_binaries": {"meow": null}}),
        serde_json::json!({"core_kind": "meow", "unknown_preference": true}),
    ] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn retired_settings_migration_still_rejects_duplicate_json_fields() {
    let path = test_path("duplicate-retired-preferences");
    let store = AppPreferencesStore::new(&path);
    for bytes in [
        r#"{"appearance":"dark","appearance":"light","core_binaries":{"meow":null}}"#,
        r#"{"core_kind":"meow","core_binaries":{"mihomo":"/first","mihomo":"/second"}}"#,
        r#"{"core_kind":"meow","core_kind":"mihomo","core_binaries":{"meow":null}}"#,
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn saves_and_loads_preferences_atomically() {
    let path = test_path("roundtrip");
    let store = AppPreferencesStore::new(&path);
    let preferences = AppPreferences {
        core_kind: CoreKind::Mihomo,
        core_binaries: CoreBinaryPreferences {
            mihomo: Some(PathBuf::from("/opt/zenclash/mihomo")),
        },
        last_known_good_core: Some(CoreKind::Mihomo),
        last_known_good_binary: Some(PathBuf::from("/opt/zenclash/mihomo")),
        appearance: AppearancePreference::Light,
        language: LanguagePreference::En,
        traffic_tray_visible: false,
        ..AppPreferences::default()
    };

    store.save(&preferences).unwrap();

    assert_eq!(store.load().unwrap(), preferences);
    fs::remove_file(path).unwrap();
}

#[test]
fn replace_refuses_to_overwrite_a_newer_preference_snapshot() {
    let path = test_path("replace-conflict");
    let store = AppPreferencesStore::new(&path);
    let expected = store.load().unwrap();
    let mut newer = expected.clone();
    newer.appearance = AppearancePreference::Dark;
    store.save(&newer).unwrap();
    let mut stale_next = expected.clone();
    stale_next.traffic_tray_visible = false;

    assert!(matches!(
        store.replace(&expected, &stale_next),
        Err(AppPreferencesError::ConcurrentModification)
    ));
    assert_eq!(store.load().unwrap(), newer);
    fs::remove_file(path).unwrap();
}

#[test]
fn update_preserves_fields_committed_by_another_store_clone() {
    let path = test_path("update-latest");
    let store = AppPreferencesStore::new(&path);
    let other_writer = store.clone();

    other_writer
        .update(|preferences| preferences.appearance = AppearancePreference::Dark)
        .unwrap();
    let updated = store
        .update(|preferences| preferences.traffic_tray_visible = false)
        .unwrap();

    assert_eq!(updated.appearance, AppearancePreference::Dark);
    assert!(!updated.traffic_tray_visible);
    assert_eq!(store.load().unwrap(), updated);
    fs::remove_file(path).unwrap();
}

#[test]
fn rejects_unknown_fields_instead_of_losing_newer_settings() {
    let path = test_path("unknown");
    fs::write(
        &path,
        r#"{"version":1,"appearance":"dark","traffic_tray_visible":true,"traffic_history_enabled":true,"traffic_retention_days":30,"future":1}"#,
    )
    .unwrap();
    let store = AppPreferencesStore::new(&path);

    assert!(matches!(store.load(), Err(AppPreferencesError::Json(_))));
    fs::remove_file(path).unwrap();
}

#[test]
fn quarantines_invalid_preferences_and_recovers_defaults() {
    let path = test_path("quarantine-invalid");
    fs::write(&path, b"{ definitely-not-json").unwrap();
    let store = AppPreferencesStore::new(&path);

    let quarantine = store.quarantine_invalid_preferences().unwrap().unwrap();

    assert!(!path.exists());
    assert_eq!(fs::read(&quarantine).unwrap(), b"{ definitely-not-json");
    assert_eq!(store.load().unwrap(), AppPreferences::default());
    fs::remove_file(quarantine).unwrap();
}

#[test]
fn valid_preferences_are_not_quarantined() {
    let path = test_path("quarantine-valid");
    let store = AppPreferencesStore::new(&path);
    store.save(&AppPreferences::default()).unwrap();

    assert_eq!(store.quarantine_invalid_preferences().unwrap(), None);
    assert!(path.exists());
    fs::remove_file(path).unwrap();
}

#[test]
fn rejects_invalid_traffic_retention() {
    let path = test_path("invalid-retention");
    let store = AppPreferencesStore::new(&path);
    let preferences = AppPreferences {
        traffic_retention_days: 0,
        ..AppPreferences::default()
    };

    assert!(matches!(
        store.save(&preferences),
        Err(AppPreferencesError::Invalid(_))
    ));
    assert!(!path.exists());
}

#[test]
fn rejects_unnormalized_system_proxy_bypass_rules() {
    let path = test_path("invalid-bypass");
    let store = AppPreferencesStore::new(&path);
    let preferences = AppPreferences {
        system_proxy_bypass: vec![" localhost ".into(), "LOCALHOST".into()],
        ..AppPreferences::default()
    };

    assert!(matches!(
        store.save(&preferences),
        Err(AppPreferencesError::Invalid(_))
    ));
    assert!(!path.exists());
}

#[test]
fn migrates_preferences_written_before_traffic_history_fields() {
    let path = test_path("legacy");
    fs::write(
        &path,
        r#"{"version":1,"appearance":"light","traffic_tray_visible":false}"#,
    )
    .unwrap();
    let store = AppPreferencesStore::new(&path);

    let preferences = store.load().unwrap();

    assert!(preferences.traffic_history_enabled);
    assert_eq!(preferences.language, LanguagePreference::ZhCn);
    assert_eq!(preferences.core_kind, CoreKind::Mihomo);
    assert_eq!(preferences.traffic_retention_days, 30);
    assert!(preferences.log_file_enabled);
    assert_eq!(preferences.log_file_max_mebibytes, 10);
    assert_eq!(preferences.network_ip_provider, PublicIpProvider::IpSb);
    assert_eq!(
        preferences.network_probe_route,
        NetworkProbeRoutePreference::Mihomo
    );
    assert!(preferences.network_latency_targets.is_empty());
    assert_eq!(
        preferences.system_proxy_bypass,
        crate::default_system_proxy_bypass()
    );
    assert_eq!(preferences.system_proxy_mode, SystemProxyMode::Manual);
    assert!(!preferences.system_proxy_enabled);
    assert!(preferences.system_proxy_ownership.is_none());
    assert_eq!(preferences.system_proxy_host, "127.0.0.1");
    assert_eq!(
        preferences.system_proxy_pac_script,
        crate::default_pac_script()
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn rejects_invalid_log_file_limit() {
    let path = test_path("invalid-log-limit");
    let store = AppPreferencesStore::new(&path);
    let preferences = AppPreferences {
        log_file_max_mebibytes: 0,
        ..AppPreferences::default()
    };

    assert!(matches!(
        store.save(&preferences),
        Err(AppPreferencesError::Invalid(_))
    ));
}

#[test]
fn rejects_duplicate_network_latency_targets() {
    let path = test_path("duplicate-network-targets");
    let store = AppPreferencesStore::new(&path);
    let target = NetworkLatencyTarget::new("One", "https://example.com/ping").unwrap();
    let preferences = AppPreferences {
        network_latency_targets: vec![target.clone(), target],
        ..AppPreferences::default()
    };

    assert!(matches!(
        store.save(&preferences),
        Err(AppPreferencesError::Invalid(_))
    ));
}
