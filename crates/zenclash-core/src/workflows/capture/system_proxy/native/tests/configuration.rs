use super::*;

fn manual_settings() -> SystemProxySettings {
    SystemProxySettings {
        mode: SystemProxyMode::Manual,
        host: " 127.0.0.1 ".into(),
        bypass: vec![" localhost ".into(), "LOCALHOST".into()],
        // Manual writes have always ignored the stored PAC draft.
        pac_script: "invalid unused PAC draft".into(),
    }
}

#[test]
fn typed_configuration_matches_legacy_manual_and_disabled_native_results() {
    let typed = Fixture::new();
    let legacy = Fixture::new();
    let settings = manual_settings();
    let claim = typed
        .controller
        .apply_configuration(SystemProxyConfiguration::enabled(&settings, 7890))
        .unwrap();
    legacy
        .controller
        .set_enabled(
            true,
            settings.mode,
            &settings.host,
            7890,
            &settings.bypass,
            &settings.pac_script,
        )
        .unwrap();
    let actual = typed
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    let old_actual = legacy
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    assert!(actual.same_state(&old_actual).unwrap());
    assert!(
        matches!(claim, Some(SystemProxyOwnership::Manual { host, port: 7890, bypass, .. })
        if host == "127.0.0.1" && bypass == ["localhost"])
    );
    assert_eq!(settings.host, " 127.0.0.1 ");
    assert_eq!(
        typed
            .controller
            .apply_configuration(SystemProxyConfiguration::disabled())
            .unwrap(),
        None
    );
    legacy
        .controller
        .set_enabled(
            false,
            SystemProxyMode::Pac,
            "bad host",
            0,
            &["bad bypass\n".into()],
            "bad PAC",
        )
        .unwrap();
    assert!(
        typed
            .backend
            .capture(NativeProxyTarget::Service("Wi-Fi"))
            .unwrap()
            .same_state(
                &legacy
                    .backend
                    .capture(NativeProxyTarget::Service("Wi-Fi"))
                    .unwrap()
            )
            .unwrap()
    );
}

#[test]
fn typed_configuration_uses_the_existing_operation_and_returns_verified_claim() {
    let fixture = Fixture::new();
    let settings = manual_settings();
    let operation = fixture.controller.begin_operation();
    let claim = operation
        .apply_configuration(SystemProxyConfiguration::enabled(&settings, 8080))
        .unwrap()
        .unwrap();
    assert!(
        matches!(claim, SystemProxyOwnership::Manual { host, port: 8080, .. } if host == "127.0.0.1")
    );
    assert_eq!(fixture.backend.0.lock().writes, ["Wi-Fi"]);
    assert!(
        operation
            .apply_configuration(SystemProxyConfiguration::disabled())
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .backend
            .capture(NativeProxyTarget::Service("Wi-Fi"))
            .unwrap()
            .summary()
            .active()
    );
}

#[test]
fn typed_invalid_manual_port_is_rejected_before_capture_or_native_write() {
    let fixture = Fixture::new();
    fixture.backend.0.lock().fail_read = true;
    let error = fixture
        .controller
        .apply_configuration(SystemProxyConfiguration::enabled(&manual_settings(), 0))
        .unwrap_err();
    assert!(matches!(
        error,
        MihomoError::SystemProxy(ref source) if matches!(source.as_ref(), SystemProxyError::Validation {
            field: SystemProxySetting::Port,
            reason: SystemProxyValidationError::ZeroPort,
        })
    ));
    assert!(fixture.backend.0.lock().writes.is_empty());
    assert!(fixture.backend.0.lock().fail_read);
}

#[test]
fn typed_pac_partial_failure_rolls_back_then_manual_replacement_retires_the_listener() {
    let fixture = Fixture::new();
    let settings = SystemProxySettings {
        mode: SystemProxyMode::Pac,
        host: "127.0.0.1".into(),
        bypass: Vec::new(),
        pac_script: default_pac_script().into(),
    };
    let before = fixture
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    fixture
        .backend
        .0
        .lock()
        .faults
        .push_back(Fault::PartialWrite);
    assert!(
        fixture
            .controller
            .apply_configuration(SystemProxyConfiguration::enabled(&settings, 7890))
            .is_err()
    );
    assert!(
        fixture
            .backend
            .capture(NativeProxyTarget::Service("Wi-Fi"))
            .unwrap()
            .same_state(&before)
            .unwrap()
    );
    assert!(fixture.controller.pac_status().is_none());
    let claim = fixture
        .controller
        .apply_configuration(SystemProxyConfiguration::enabled(&settings, 7890))
        .unwrap();
    let listener = fixture.controller.pac_status().unwrap();
    assert!(matches!(claim, Some(SystemProxyOwnership::Pac { url, .. }) if url == listener.url));
    fixture
        .controller
        .apply_configuration(SystemProxyConfiguration::enabled(&manual_settings(), 7890))
        .unwrap();
    assert!(fixture.controller.pac_status().is_none());
}
