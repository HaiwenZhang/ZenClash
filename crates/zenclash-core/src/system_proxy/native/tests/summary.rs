use super::*;

#[test]
fn identical_protocol_summaries_do_not_authorize_restoring_different_native_attributes() {
    let fixture = Fixture::new();
    let before = fixture
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    fixture
        .backend
        .0
        .lock()
        .attributes
        .entry("Wi-Fi".into())
        .or_default()
        .insert(
            "native-hidden-setting".into(),
            b"external replacement".to_vec(),
        );
    let after = fixture
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    assert_eq!(before.summary(), after.summary());
    assert!(!before.same_state(&after).unwrap());
    assert!(!NativeMutationWitness::new(&before).permits(&after).unwrap());
}

#[test]
fn session_uses_readonly_display_facts_without_replacing_native_ownership_snapshot() {
    let fixture = Fixture::new();
    let cached = SystemProxySummary {
        service: "Wi-Fi".into(),
        auto_enabled: true,
        auto_url: "http://cached.test/proxy.pac".into(),
        ..Default::default()
    };
    let mut display = cached.clone();
    display.auto_enabled = false;
    {
        let mut state = fixture.backend.0.lock();
        state.services.insert("Wi-Fi".into(), cached.clone());
        state.display_override = Some(display.clone());
    }
    let raw_before = fixture
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    let snapshot = fixture.session.snapshot().unwrap();
    let raw_after = fixture
        .backend
        .capture(NativeProxyTarget::Service("Wi-Fi"))
        .unwrap();
    assert_eq!(snapshot.actual, display);
    assert_eq!(snapshot.ownership, SystemProxyOwnershipState::Unowned);
    assert_eq!(raw_after.summary(), &cached);
    assert!(raw_before.same_state(&raw_after).unwrap());
    assert!(fixture.backend.0.lock().writes.is_empty());
}

#[test]
fn failed_readonly_display_does_not_publish_legacy_native_summary() {
    let fixture = Fixture::new();
    fixture.backend.0.lock().fail_display = true;
    let error = fixture.session.snapshot().unwrap_err();
    assert!(
        matches!(error, SystemProxySessionError::Native(MihomoError::SystemProxy(source))
        if matches!(source.as_ref(), SystemProxyError::ObservationUnstable))
    );
    assert!(fixture.backend.0.lock().writes.is_empty());
}
