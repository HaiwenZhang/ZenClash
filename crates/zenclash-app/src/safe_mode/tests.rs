use std::cell::Cell;

use super::*;

#[test]
fn safe_mode_is_disabled_by_default() {
    let mode = SafeMode::from_inputs(["--other"], None);
    assert!(!mode.is_enabled());
    assert!(mode.allows_system_proxy());
    assert!(mode.allows_privileged_service());
    assert!(mode.allows_tun());
    assert!(mode.allows_autostart());
    assert!(mode.allows_system_dns());
    assert!(mode.allows_default_ports());
}

#[test]
fn safe_mode_activates_from_cli_flag() {
    let mode = SafeMode::from_inputs(["--continue-local", SAFE_MODE_CLI_FLAG], None);
    assert!(mode.is_enabled());
    assert!(!mode.allows_system_proxy());
    assert!(!mode.allows_privileged_service());
    assert!(!mode.allows_tun());
    assert!(!mode.allows_autostart());
    assert!(!mode.allows_system_dns());
    assert!(!mode.allows_default_ports());
}

#[test]
fn safe_mode_activates_only_for_truthy_environment_values() {
    for truthy in ["1", "true", "TRUE", " yes ", "On"] {
        let mode = SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new(truthy)));
        assert!(mode.is_enabled(), "expected {truthy:?} to enable Safe Mode");
    }
    for falsy in ["", "0", "false", "no", "off", "2"] {
        let mode = SafeMode::from_inputs(std::iter::empty::<&str>(), Some(OsStr::new(falsy)));
        assert!(
            !mode.is_enabled(),
            "expected {falsy:?} to keep Safe Mode disabled"
        );
    }
}

#[test]
fn safe_mode_isolates_data_root_and_instance_lock_paths() {
    let base = Path::new("/data/zenclash");
    assert_eq!(SafeMode::disabled().data_root(base), base);
    assert_eq!(
        SafeMode::disabled().instance_lock_path(base),
        base.join("instance.lock")
    );

    let safe = SafeMode::enabled();
    assert_eq!(safe.data_root(base), base.join("safe-mode"));
    assert_eq!(
        safe.instance_lock_path(base),
        base.join("safe-mode").join("instance.lock")
    );
}

#[test]
fn safe_mode_guard_short_circuits_host_side_effects() {
    let called = Cell::new(false);
    let skipped = SafeMode::enabled()
        .guard_host_side_effect("set_system_proxy", || {
            called.set(true);
            Ok::<_, String>(42)
        })
        .unwrap();
    assert_eq!(skipped, None);
    assert!(!called.get());

    let executed = SafeMode::disabled()
        .guard_host_side_effect("set_system_proxy", || {
            called.set(true);
            Ok::<_, String>(42)
        })
        .unwrap();
    assert_eq!(executed, Some(42));
    assert!(called.get());
}
