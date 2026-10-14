use super::*;
use crate::tun_runtime::test_support::{CommandFixture, output};

#[test]
fn an_existing_down_device_does_not_become_active_when_its_route_matches() {
    let mut fixture = CommandFixture::new([
        Ok(output(
            b"utun42: flags=8050<POINTOPOINT,RUNNING,MULTICAST> mtu 1380\n",
            true,
        )),
        Ok(output(b"interface: utun42\n", true)),
    ]);
    let observed = observe_with("utun42", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Inactive);
    assert_eq!(observed.route, CapabilityState::Active);
    assert!(observed.detail.contains("device utun42 inactive"));
    assert_eq!(
        fixture.calls[0],
        ("/sbin/ifconfig".into(), vec!["utun42".to_owned()])
    );
    assert_eq!(fixture.calls.len(), 2);
}

#[test]
fn an_up_virtual_device_does_not_require_carrier_or_operational_up() {
    let mut fixture = CommandFixture::new([
        Ok(output(
            b"utun42: flags=8011<UP,POINTOPOINT,MULTICAST> mtu 1380\n",
            true,
        )),
        Ok(output(b"interface: utun42\n", true)),
    ]);
    let observed = observe_with("utun42", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Active);
    assert_eq!(observed.route, CapabilityState::Active);
    assert_eq!(observed.device_name.as_deref(), Some("utun42"));
}

#[test]
fn unrelated_malformed_or_failed_device_output_remains_unknown() {
    for reply in [
        output(b"utun43: flags=1<UP>\n", true),
        output(b"device present, UP\n", true),
        output(b"", true),
        output(b"\xff", true),
        output(
            b"utun42: flags=8011<UP,POINTOPOINT,MULTICAST> mtu 1380\n",
            false,
        ),
    ] {
        let mut fixture =
            CommandFixture::new([Ok(reply), Ok(output(b"interface: utun42\n", true))]);
        let observed = observe_with("utun42", |command, args| fixture.read(command, args));
        assert_eq!(observed.device, CapabilityState::Unknown);
        assert_eq!(observed.route, CapabilityState::Active);
    }
}

#[test]
fn a_device_reader_failure_preserves_independent_route_evidence() {
    let mut fixture = CommandFixture::new([
        Err("permission/readback failed".into()),
        Ok(output(b"interface: utun42\n", true)),
    ]);
    let observed = observe_with("utun42", |command, args| fixture.read(command, args));
    assert_eq!(observed.device, CapabilityState::Unknown);
    assert_eq!(observed.route, CapabilityState::Active);
    assert!(observed.detail.contains("unknown"));
}
