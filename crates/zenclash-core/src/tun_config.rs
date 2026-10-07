// Adapted from Clash Verge Rev enhance/tun.rs, config/clash.rs and utils/init.rs
// on 2026-10-07. GPL-3.0-only; see docs/research/clash-verge-tun-implementation.md.
//! One TUN projection for local YAML, service staging and frozen recovery bundles.

use serde_yaml::{Mapping, Value, mapping::Entry};

pub(crate) fn normalize(root: &mut Mapping) -> bool {
    let ipv6 = root.get("ipv6").and_then(Value::as_bool).unwrap_or(false);
    let Some(tun) = root.get_mut("tun").and_then(Value::as_mapping_mut) else {
        return false;
    };
    if tun.get("enable").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let mut changed = false;
    for (key, value) in [
        ("stack", Value::from("gvisor")),
        ("auto-route", Value::from(true)),
        ("strict-route", Value::from(false)),
        ("auto-detect-interface", Value::from(true)),
        ("dns-hijack", strings(&["any:53"])),
        (
            "device",
            Value::from(if cfg!(target_os = "macos") {
                "utun1024"
            } else {
                "Mihomo"
            }),
        ),
        ("mtu", Value::from(1500)),
    ] {
        changed |= default(tun, key, value);
    }
    let dns = root
        .entry(Value::from("dns"))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    // Keep malformed user fields for the configuration validator to reject.
    let Some(dns) = dns.as_mapping_mut() else {
        return changed;
    };
    if dns
        .get("enhanced-mode")
        .and_then(Value::as_str)
        .unwrap_or("fake-ip")
        != "fake-ip"
    {
        return changed;
    }
    changed |= set(dns, "enable", Value::from(true));
    changed |= set(dns, "ipv6", Value::from(ipv6));
    changed |= default(dns, "enhanced-mode", Value::from("fake-ip"));
    changed |= default(dns, "fake-ip-range", Value::from("198.18.0.1/16"));
    if ipv6 {
        changed |= default(dns, "fake-ip-range6", Value::from("2001:2::0/64"));
    }
    // Bare bundled/recovery profiles have no resolver. Use the reference app's
    // DNS template only for absent fields, preserving subscription resolvers.
    changed |= default(
        dns,
        "nameserver",
        strings(&[
            "8.8.8.8",
            "https://doh.pub/dns-query",
            "https://dns.alidns.com/dns-query",
        ]),
    );
    changed |= default(
        dns,
        "default-nameserver",
        strings(&[
            "system",
            "223.6.6.6",
            "8.8.8.8",
            "2400:3200::1",
            "2001:4860:4860::8888",
        ]),
    );
    changed
}

fn strings(values: &[&str]) -> Value {
    Value::Sequence(values.iter().map(|value| Value::from(*value)).collect())
}

fn default(mapping: &mut Mapping, key: &str, value: Value) -> bool {
    if let Entry::Vacant(entry) = mapping.entry(Value::from(key)) {
        entry.insert(value);
        true
    } else {
        false
    }
}

fn set(mapping: &mut Mapping, key: &str, value: Value) -> bool {
    if mapping.get(key) == Some(&value) {
        return false;
    }
    mapping.insert(Value::from(key), value);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(yaml: &str) -> Value {
        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        normalize(value.as_mapping_mut().unwrap());
        value
    }

    #[test]
    fn bare_tun_has_routes_dns_capture_and_a_working_resolver_configuration() {
        let yaml = project("tun: {enable: true}\n");
        assert_eq!(yaml["tun"]["stack"], "gvisor");
        assert_eq!(yaml["tun"]["auto-route"], true);
        assert_eq!(yaml["tun"]["auto-detect-interface"], true);
        assert_eq!(yaml["tun"]["dns-hijack"][0], "any:53");
        assert_eq!(yaml["dns"]["enable"], true);
        assert_eq!(yaml["dns"]["enhanced-mode"], "fake-ip");
        assert_eq!(yaml["dns"]["fake-ip-range"], "198.18.0.1/16");
        assert!(!yaml["dns"]["nameserver"].as_sequence().unwrap().is_empty());
        assert!(yaml["dns"]["fake-ip-range6"].is_null());
    }

    #[test]
    fn fake_ip_ipv6_follows_top_level_switch_and_preserves_custom_ranges() {
        let yaml = project("ipv6: true\ntun: {enable: true}\ndns: {ipv6: false}\n");
        assert_eq!(yaml["dns"]["ipv6"], true);
        assert_eq!(yaml["dns"]["fake-ip-range6"], "2001:2::0/64");
        let yaml = project(
            "ipv6: true\ntun: {enable: true, stack: mixed, auto-route: false, dns-hijack: []}\ndns: {fake-ip-range: '198.19.0.1/16', fake-ip-range6: 'fc00::/64', nameserver: [9.9.9.9]}\n",
        );
        assert_eq!(yaml["tun"]["stack"], "mixed");
        assert_eq!(yaml["tun"]["auto-route"], false);
        assert!(yaml["tun"]["dns-hijack"].as_sequence().unwrap().is_empty());
        assert_eq!(yaml["dns"]["fake-ip-range"], "198.19.0.1/16");
        assert_eq!(yaml["dns"]["fake-ip-range6"], "fc00::/64");
        assert_eq!(yaml["dns"]["nameserver"][0], "9.9.9.9");
    }

    #[test]
    fn deliberate_redir_host_and_disabled_tun_leave_dns_untouched() {
        for yaml in [
            "tun: {enable: true}\ndns: {enable: false, ipv6: true, enhanced-mode: redir-host}\n",
            "tun: {enable: false}\ndns: {enable: false, ipv6: true}\n",
        ] {
            let before: Value = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(project(yaml)["dns"], before["dns"]);
        }
    }

    #[test]
    fn normalization_is_idempotent_and_does_not_hide_invalid_dns() {
        let mut yaml = project("tun: {enable: true}\n");
        assert!(!normalize(yaml.as_mapping_mut().unwrap()));
        assert_eq!(
            project("tun: {enable: true}\ndns: invalid\n")["dns"],
            "invalid"
        );
    }
}
