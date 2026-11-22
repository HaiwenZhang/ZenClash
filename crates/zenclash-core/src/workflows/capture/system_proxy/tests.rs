use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use super::{
    SystemProxyController, SystemProxyManager, SystemProxyMode, SystemProxyOwnership,
    SystemProxyReconcileDecision, SystemProxyReleaseReason, SystemProxySession,
    SystemProxySessionError, SystemProxySettings, SystemProxyStatus, decide_system_proxy_reconcile,
    normalize_system_proxy_bypass, normalize_system_proxy_host, system_proxy_summary_matches_claim,
};
use crate::{AppPreferences, AppPreferencesStore};

#[test]
fn controller_clones_serialize_complete_native_operations() {
    let controller = Arc::new(SystemProxyController::default());
    let (first_acquired, first_acquired_rx) = mpsc::channel();
    let (release_first, release_first_rx) = mpsc::channel();
    let first = {
        let controller = Arc::clone(&controller);
        thread::spawn(move || {
            let _operation = controller.begin_operation();
            first_acquired.send(()).unwrap();
            release_first_rx.recv().unwrap();
        })
    };
    first_acquired_rx.recv().unwrap();

    let (second_acquired, second_acquired_rx) = mpsc::channel();
    let second = {
        let controller = Arc::clone(&controller);
        thread::spawn(move || {
            let _operation = controller.begin_operation();
            second_acquired.send(()).unwrap();
        })
    };

    assert!(
        second_acquired_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );
    release_first.send(()).unwrap();
    second_acquired_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    first.join().unwrap();
    second.join().unwrap();
}

#[test]
fn rejects_invalid_proxy_endpoint_before_platform_command() {
    let manager = SystemProxyManager {
        service: "test-service".into(),
    };

    assert!(manager.set_enabled(true, "   ", 7890).is_err());
    assert!(manager.set_enabled(true, "127.0.0.1", 0).is_err());
}

#[test]
fn bypass_normalization_trims_ignores_empty_and_deduplicates() {
    let normalized = normalize_system_proxy_bypass(&[
        " localhost ".into(),
        String::new(),
        "LOCALHOST".into(),
        "192.168.0.0/16".into(),
        "*.example.com".into(),
    ])
    .unwrap();

    assert_eq!(normalized, ["localhost", "192.168.0.0/16", "*.example.com"]);
}

#[test]
fn bypass_normalization_rejects_command_and_gvariant_delimiters() {
    let error = normalize_system_proxy_bypass(&["localhost';evil".into()]).unwrap_err();

    let expected = zenclash_i18n::text("system_proxy.validation.invalid_syntax");
    assert!(error.to_string().contains(&expected));
}

#[test]
fn bypass_normalization_rejects_invalid_cidr_prefix() {
    let error = normalize_system_proxy_bypass(&["192.168.0.0/99".into()]).unwrap_err();

    let expected = zenclash_i18n::text("system_proxy.validation.invalid_syntax");
    assert!(error.to_string().contains(&expected));
}

#[test]
fn bypass_normalization_accepts_a_wildcard_dns_label_but_not_empty_labels() {
    assert_eq!(
        normalize_system_proxy_bypass(&["*.example.com".into()]).unwrap(),
        ["*.example.com"]
    );
    assert!(normalize_system_proxy_bypass(&["example..com".into()]).is_err());
}

#[test]
fn system_proxy_host_accepts_loopback_and_rejects_unspecified_addresses() {
    assert_eq!(
        normalize_system_proxy_host(" localhost ").unwrap(),
        "localhost"
    );
    assert!(normalize_system_proxy_host("0.0.0.0").is_err());
}

#[test]
fn ownership_requires_the_complete_native_state_to_still_match() {
    let ownership = SystemProxyOwnership::Manual {
        service: "Wi-Fi".into(),
        host: "127.0.0.1".into(),
        port: 7890,
        bypass: vec!["localhost".into()],
    };
    let owned = SystemProxyStatus {
        service: "Wi-Fi".into(),
        enabled: true,
        server: "127.0.0.1".into(),
        port: 7890,
        secure_enabled: true,
        secure_server: "127.0.0.1".into(),
        secure_port: 7890,
        bypass: vec!["localhost".into()],
        ..SystemProxyStatus::default()
    };
    let mut replaced = owned.clone();
    replaced.port = 8080;

    assert!(system_proxy_summary_matches_claim(
        "Wi-Fi", &owned, &ownership
    ));
    assert!(!system_proxy_summary_matches_claim(
        "Wi-Fi", &replaced, &ownership
    ));
}

#[test]
fn reconciliation_preserves_external_replacement_and_recovers_after_core_restart() {
    let ownership = SystemProxyOwnership::Manual {
        service: "Wi-Fi".into(),
        host: "127.0.0.1".into(),
        port: 7890,
        bypass: vec!["localhost".into()],
    };
    let owned = SystemProxyStatus {
        service: "Wi-Fi".into(),
        enabled: true,
        server: "127.0.0.1".into(),
        port: 7890,
        secure_enabled: true,
        secure_server: "127.0.0.1".into(),
        secure_port: 7890,
        bypass: vec!["localhost".into()],
        ..SystemProxyStatus::default()
    };
    let mut preferences = AppPreferences {
        system_proxy_enabled: true,
        system_proxy_ownership: Some(ownership),
        ..AppPreferences::default()
    };

    assert_eq!(
        decide_system_proxy_reconcile(&preferences, false, None, None),
        SystemProxyReconcileDecision::Release(SystemProxyReleaseReason::CoreUnavailable)
    );

    preferences.system_proxy_ownership = None;
    assert_eq!(
        decide_system_proxy_reconcile(
            &preferences,
            true,
            Some(7890),
            Some(("Wi-Fi", &SystemProxyStatus::default())),
        ),
        SystemProxyReconcileDecision::Restore
    );

    let mut external = owned;
    external.port = 8080;
    assert_eq!(
        decide_system_proxy_reconcile(&preferences, true, Some(7890), Some(("Wi-Fi", &external)),),
        SystemProxyReconcileDecision::OwnershipLost
    );
}

#[test]
fn inactive_settings_are_normalized_and_persisted_without_native_writes() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-system-proxy-settings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = AppPreferencesStore::new(root.join("preferences.json"));
    let session = SystemProxySession::new(store.clone(), SystemProxyController::default());

    let saved = session
        .save_settings(
            SystemProxySettings {
                mode: SystemProxyMode::Manual,
                host: " localhost ".into(),
                bypass: vec![" localhost ".into(), "LOCALHOST".into()],
                pac_script: super::default_pac_script().into(),
            },
            0,
        )
        .unwrap();

    assert!(!saved.system_proxy_enabled);
    assert_eq!(saved.system_proxy_host, "localhost");
    assert_eq!(saved.system_proxy_bypass, ["localhost"]);
    assert_eq!(store.load().unwrap(), saved);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn enabled_intent_without_a_port_is_rejected_before_native_or_persistent_change() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-system-proxy-port-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = AppPreferencesStore::new(root.join("preferences.json"));
    let session = SystemProxySession::new(store.clone(), SystemProxyController::default());

    let error = session.set_enabled(true, 0).unwrap_err();

    assert!(matches!(error, SystemProxySessionError::MissingPort));
    assert!(!store.load().unwrap().system_proxy_enabled);
}
