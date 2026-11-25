use super::*;

#[test]
fn dropping_an_older_tray_token_does_not_clear_a_newer_sender() {
    let old_token = next_tray_token();
    let new_token = next_tray_token();
    assert_ne!(old_token, new_token);

    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let (new_tx, _new_rx) = mpsc::unbounded_channel();

    register_native_event_sender(old_token, old_tx);
    register_native_event_sender(new_token, new_tx);

    unregister_native_event_sender(old_token);
    assert!(
        current_native_event_sender().is_some(),
        "dropping an outdated tray icon must not clear the active tray sender"
    );

    unregister_native_event_sender(new_token);
    assert!(
        current_native_event_sender().is_none(),
        "dropping the current tray icon must clear the active tray sender"
    );
}

#[test]
fn tray_menu_state_equality_detects_real_menu_changes() {
    let base = TrayMenuState {
        mode: "rule".into(),
        system_proxy: true,
        tun: false,
        mixed_port: 7890,
        profile_name: "Primary".into(),
        profiles: vec![TrayProfile {
            id: "p1".into(),
            name: "Primary".into(),
            active: true,
        }],
        groups: vec![TrayProxyGroup {
            controller_name: "Proxy".into(),
            selectable: true,
            automatic: false,
            has_more: false,
            name: "Proxy".into(),
            now: "HK-01".into(),
            test_url: None,
            proxies: vec![TrayProxyNode {
                name: "HK-01".into(),
                provider: None,
                delay: Some(42),
            }]
            .into(),
        }],
        directories: Vec::new(),
    };
    let identical = base.clone();
    let mut changed = base.clone();
    changed.tun = true;

    assert_eq!(base, identical);
    assert_ne!(base, changed);
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
fn native_right_click_menu_does_not_require_manual_reentry() {
    assert!(native_menu_on_right_click());
    assert_eq!(click_action(MouseButton::Right), None);
}

#[test]
fn primary_tray_click_shows_the_main_window() {
    assert_eq!(click_action(MouseButton::Left), Some(TrayClick::ShowWindow));
    assert_eq!(click_action(MouseButton::Middle), None);
}

#[cfg(target_os = "macos")]
#[test]
fn secondary_tray_click_requests_the_native_menu() {
    assert!(!native_menu_on_right_click());
    assert_eq!(click_action(MouseButton::Right), Some(TrayClick::ShowMenu));
}
