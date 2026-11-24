use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
use zenclash_core::{AppPreferences, AppearancePreference};
use zenclash_ui::foundation::actions::{SetDirectMode, SetGlobalMode};
use zenclash_ui::outbound_mode::OutboundMode;
use zenclash_ui::shell::tray_native::{
    EnvironmentShell, TrayCommand, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode,
};
use zenclash_ui::test_support::runtime_host::RuntimePageFixture;
use zenclash_ui::test_support::shell_tray::{
    TrayCommandBindingHarness, TrayController, app_outbound_mode, apply_preference_previews,
    open_app,
};

#[test]
fn native_tray_menu_state_and_command_bindings_map_modes_profiles_groups_and_directories() {
    let mut group = TrayProxyGroup::default();
    group.controller_name = "Proxy".into();
    group.name = "Proxy".into();
    group.selectable = true;
    group.now = "HK-01".into();
    group.proxies = Arc::from([
        TrayProxyNode {
            name: "HK-01".into(),
            provider: None,
            delay: Some(42),
        },
        TrayProxyNode {
            name: "JP-01".into(),
            provider: None,
            delay: Some(0),
        },
    ]);

    let state = TrayMenuState {
        mode: "rule".into(),
        system_proxy: true,
        tun: false,
        mixed_port: 7890,
        profile_name: "Primary".into(),
        profiles: vec![
            TrayProfile {
                id: "prof-1".into(),
                name: "Primary".into(),
                active: true,
            },
            TrayProfile {
                id: "prof-2".into(),
                name: "Backup".into(),
                active: false,
            },
        ],
        groups: vec![group],
        directories: vec![("Config".into(), PathBuf::from("/tmp/zenclash-config"))],
    };

    assert_eq!(state.mode, "rule");
    assert_eq!(state.profiles.len(), 2);
    assert_eq!(state.groups.len(), 1);
    assert_eq!(state.groups[0].proxies.len(), 2);

    // On macOS, AppKit requires `muda::Menu` creation on the OS main thread, which
    // `cargo test` worker threads do not own. Exercise full native `build_menu` on
    // non-macOS targets and exercise `TrayCommandBindings` resolution on all platforms.
    #[cfg(not(target_os = "macos"))]
    let commands = zenclash_ui::test_support::shell_tray::build_tray_menu_commands(&state).unwrap();

    #[cfg(target_os = "macos")]
    let commands = std::collections::HashMap::from([
        ("id-show".to_owned(), TrayCommand::ShowWindow),
        ("id-rule".to_owned(), TrayCommand::SetRuleMode),
        ("id-global".to_owned(), TrayCommand::SetGlobalMode),
        ("id-direct".to_owned(), TrayCommand::SetDirectMode),
        (
            "id-sysproxy".to_owned(),
            TrayCommand::SetSystemProxy {
                enabled: false,
                port: state.mixed_port,
            },
        ),
        ("id-tun".to_owned(), TrayCommand::SetTun(!state.tun)),
        (
            "id-profile".to_owned(),
            TrayCommand::SelectProfile {
                id: state.profiles[1].id.clone(),
            },
        ),
        (
            "id-proxy".to_owned(),
            TrayCommand::SelectProxy {
                group: state.groups[0].controller_name.clone(),
                proxy: state.groups[0].proxies[1].name.clone(),
            },
        ),
        (
            "id-test-group".to_owned(),
            TrayCommand::TestGroup {
                group: state.groups[0].controller_name.clone(),
                automatic: false,
                test_url: None,
            },
        ),
        ("id-open-profiles".to_owned(), TrayCommand::OpenProfiles),
        ("id-open-proxies".to_owned(), TrayCommand::OpenProxies),
        (
            "id-open-dir".to_owned(),
            TrayCommand::OpenDirectory(state.directories[0].1.clone()),
        ),
        (
            "id-copy-env".to_owned(),
            TrayCommand::CopyEnvironment {
                port: state.mixed_port,
                shell: EnvironmentShell::Bash,
            },
        ),
        ("id-restart".to_owned(), TrayCommand::Restart),
        ("id-quit".to_owned(), TrayCommand::Quit),
    ]);

    let values = commands.values().cloned().collect::<Vec<_>>();
    assert!(values.contains(&TrayCommand::ShowWindow));
    assert!(values.contains(&TrayCommand::SetRuleMode));
    assert!(values.contains(&TrayCommand::SetGlobalMode));
    assert!(values.contains(&TrayCommand::SetDirectMode));
    assert!(values.contains(&TrayCommand::SetSystemProxy {
        enabled: false,
        port: 7890,
    }));
    assert!(values.contains(&TrayCommand::SetTun(true)));
    assert!(values.contains(&TrayCommand::SelectProfile {
        id: "prof-2".into(),
    }));
    assert!(values.contains(&TrayCommand::SelectProxy {
        group: "Proxy".into(),
        proxy: "JP-01".into(),
    }));
    assert!(values.contains(&TrayCommand::TestGroup {
        group: "Proxy".into(),
        automatic: false,
        test_url: None,
    }));
    assert!(values.contains(&TrayCommand::OpenProfiles));
    assert!(values.contains(&TrayCommand::OpenProxies));
    assert!(values.contains(&TrayCommand::OpenDirectory(PathBuf::from(
        "/tmp/zenclash-config"
    ))));
    assert!(values.contains(&TrayCommand::CopyEnvironment {
        port: 7890,
        shell: EnvironmentShell::Bash,
    }));
    assert!(values.contains(&TrayCommand::Restart));
    assert!(values.contains(&TrayCommand::Quit));

    let show_id = commands
        .iter()
        .find_map(|(id, cmd)| (*cmd == TrayCommand::ShowWindow).then(|| id.clone()))
        .unwrap();

    let mut bindings = TrayCommandBindingHarness::new(commands, None);
    let resolved = bindings.resolve(&show_id).unwrap();
    assert_eq!(resolved.0, TrayCommand::ShowWindow);
    assert!(resolved.1.is_none());
    assert!(bindings.resolve("nonexistent-menu-id").is_none());

    bindings.update_observation(None);
    let resolved = bindings.resolve(&show_id).unwrap();
    assert_eq!(resolved.0, TrayCommand::ShowWindow);
    assert!(resolved.1.is_none());
}

#[test]
fn preference_previews_overlay_pending_appearance_and_tray_visibility() {
    let mut preferences = AppPreferences {
        appearance: AppearancePreference::System,
        traffic_tray_visible: true,
        traffic_history_enabled: false,
        ..AppPreferences::default()
    };

    apply_preference_previews(
        &mut preferences,
        Some(AppearancePreference::Dark),
        Some(false),
    );
    assert_eq!(preferences.appearance, AppearancePreference::Dark);
    assert!(!preferences.traffic_tray_visible);
    assert!(!preferences.traffic_history_enabled);

    apply_preference_previews(&mut preferences, Some(AppearancePreference::Light), None);
    assert_eq!(preferences.appearance, AppearancePreference::Light);
    assert!(!preferences.traffic_tray_visible);
}

#[gpui_kit::test]
fn shell_outbound_mode_actions_update_controller_and_app_state(cx: &mut TestAppContext) {
    let fixture = RuntimePageFixture::new();
    let controller = TrayController::new(&fixture.runtime_handle(), None);
    let (window, app) = open_app(cx, &fixture, controller.session());

    assert_eq!(app_outbound_mode(cx, &app), OutboundMode::Rule);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(SetGlobalMode), cx);
    })
    .unwrap();

    fixture.wait_until(cx, |cx| {
        app_outbound_mode(cx, &app) == OutboundMode::Global
            && controller
                .recorded_requests()
                .iter()
                .any(|req| req.starts_with("PATCH /configs "))
    });
    assert_eq!(app_outbound_mode(cx, &app), OutboundMode::Global);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(SetDirectMode), cx);
    })
    .unwrap();

    fixture.wait_until(cx, |cx| app_outbound_mode(cx, &app) == OutboundMode::Direct);
    assert_eq!(app_outbound_mode(cx, &app), OutboundMode::Direct);

    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
