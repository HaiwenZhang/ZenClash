use super::*;
use crate::tun_runtime::test_support::{CommandFixture, output};

#[test]
fn an_existing_down_device_does_not_become_active_when_its_route_matches() {
    let mut fixture = CommandFixture::new([
        Ok(output(
            b"7: Mihomo: <POINTOPOINT,NOARP,LOWER_UP> mtu 1500 state DOWN\n",
            true,
        )),
        Ok(output(b"1.1.1.1 dev Mihomo src 198.18.0.1\n", true)),
    ]);
    let observed = observe_with("Mihomo", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Inactive);
    assert_eq!(observed.route, CapabilityState::Active);
    assert!(observed.detail.contains("device Mihomo inactive"));
    assert_eq!(
        fixture.calls[0],
        (
            "ip".into(),
            vec!["link".into(), "show".into(), "dev".into(), "Mihomo".into()]
        )
    );
    assert_eq!(fixture.calls.len(), 2);
}

#[test]
fn an_up_virtual_device_does_not_require_carrier_or_operational_up() {
    let mut fixture = CommandFixture::new([
        Ok(output(
            b"7: Mihomo@NONE: <POINTOPOINT,NOARP,UP> mtu 1500 state UNKNOWN\n",
            true,
        )),
        Ok(output(b"1.1.1.1 dev Mihomo src 198.18.0.1\n", true)),
    ]);
    let observed = observe_with("Mihomo", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Active);
    assert_eq!(observed.route, CapabilityState::Active);
    assert_eq!(observed.device_name.as_deref(), Some("Mihomo"));
}

#[test]
fn unrelated_malformed_or_failed_device_output_remains_unknown() {
    for reply in [
        output(b"7: Other: <UP,LOWER_UP> mtu 1500 state UP\n", true),
        output(b"device present, UP\n", true),
        output(b"", true),
        output(b"\xff", true),
        output(
            b"7: Mihomo@NONE: <POINTOPOINT,NOARP,UP> mtu 1500 state UNKNOWN\n",
            false,
        ),
    ] {
        let mut fixture = CommandFixture::new([
            Ok(reply),
            Ok(output(b"1.1.1.1 dev Mihomo src 198.18.0.1\n", true)),
        ]);
        let observed = observe_with("Mihomo", |command, args| fixture.read(command, args));
        assert_eq!(observed.device, CapabilityState::Unknown);
        assert_eq!(observed.route, CapabilityState::Active);
    }
}

#[test]
fn a_device_reader_failure_preserves_independent_route_evidence() {
    let mut fixture = CommandFixture::new([
        Err("permission/readback failed".into()),
        Ok(output(b"1.1.1.1 dev Mihomo src 198.18.0.1\n", true)),
    ]);
    let observed = observe_with("Mihomo", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Unknown);
    assert_eq!(observed.route, CapabilityState::Active);
    assert!(observed.detail.contains("unknown"));
}
