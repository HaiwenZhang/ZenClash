//! DNS, Sniffer, and TUN configuration input groups and patch builders.

use gpui_kit::Entity;
use gpui_kit::component::input::{InputState, TextareaState};
use serde_json::{Map, Value};

use super::codec::{
    config_lines, config_list_csv, config_mapping, config_number_or_empty, config_string, csv,
    insert_optional_lines, insert_optional_mapping, insert_optional_string, ports, text,
};
use super::{InputFactory, config_source};

pub(crate) struct DnsInputs {
    pub enhanced_mode: Entity<InputState>,
    pub fake_ip_range: Entity<InputState>,
    pub fake_ip_filter_mode: Entity<InputState>,
    pub fake_ip_filter: Entity<TextareaState>,
    pub default_nameserver: Entity<TextareaState>,
    pub nameserver: Entity<TextareaState>,
    pub proxy_server_nameserver: Entity<TextareaState>,
    pub direct_nameserver: Entity<TextareaState>,
    pub fallback: Entity<TextareaState>,
    pub fallback_geoip: Entity<InputState>,
    pub fallback_geoip_code: Entity<InputState>,
    pub fallback_ipcidr: Entity<TextareaState>,
    pub fallback_domain: Entity<TextareaState>,
    pub nameserver_policy: Entity<TextareaState>,
    pub hosts: Entity<TextareaState>,
    source: Value,
}

pub(crate) struct SnifferInputs {
    pub http_ports: Entity<InputState>,
    pub tls_ports: Entity<InputState>,
    pub quic_ports: Entity<InputState>,
    pub skip_domain: Entity<TextareaState>,
    pub force_domain: Entity<TextareaState>,
    pub skip_dst_address: Entity<TextareaState>,
    pub skip_src_address: Entity<TextareaState>,
    source: Value,
}

pub(crate) struct TunInputs {
    pub stack: Entity<InputState>,
    pub device: Entity<InputState>,
    pub mtu: Entity<InputState>,
    pub dns_hijack: Entity<InputState>,
    pub route_include_address: Entity<TextareaState>,
    pub route_exclude_address: Entity<TextareaState>,
    source: Value,
}

impl DnsInputs {
    pub(super) fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
        Self {
            enhanced_mode: factory.single(
                "/dns/enhanced-mode",
                config_string(config, "/dns/enhanced-mode", "redir-host"),
                "fake-ip / redir-host / normal",
            ),
            fake_ip_range: factory.single(
                "/dns/fake-ip-range",
                config_string(config, "/dns/fake-ip-range", ""),
                "198.18.0.1/16",
            ),
            fake_ip_filter_mode: factory.single(
                "/dns/fake-ip-filter-mode",
                config_string(config, "/dns/fake-ip-filter-mode", ""),
                "blacklist / whitelist / rule",
            ),
            fake_ip_filter: factory.multi(
                "/dns/fake-ip-filter",
                config_lines(config, "/dns/fake-ip-filter"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain_or_rule"),
            ),
            default_nameserver: factory.multi(
                "/dns/default-nameserver",
                config_lines(config, "/dns/default-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.one_ip_dns"),
            ),
            nameserver: factory.multi(
                "/dns/nameserver",
                config_lines(config, "/dns/nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.one_dns"),
            ),
            proxy_server_nameserver: factory.multi(
                "/dns/proxy-server-nameserver",
                config_lines(config, "/dns/proxy-server-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.proxy_resolver"),
            ),
            direct_nameserver: factory.multi(
                "/dns/direct-nameserver",
                config_lines(config, "/dns/direct-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.direct_resolver"),
            ),
            fallback: factory.multi(
                "/dns/fallback",
                config_lines(config, "/dns/fallback"),
                "Fallback DNS",
            ),
            fallback_geoip: factory.single(
                "/dns/fallback-filter/geoip",
                config
                    .pointer("/dns/fallback-filter/geoip")
                    .and_then(Value::as_bool)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "true / false",
            ),
            fallback_geoip_code: factory.single(
                "/dns/fallback-filter/geoip-code",
                config_string(config, "/dns/fallback-filter/geoip-code", ""),
                "CN",
            ),
            fallback_ipcidr: factory.multi(
                "/dns/fallback-filter/ipcidr",
                config_lines(config, "/dns/fallback-filter/ipcidr"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            fallback_domain: factory.multi(
                "/dns/fallback-filter/domain",
                config_lines(config, "/dns/fallback-filter/domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain_rule"),
            ),
            nameserver_policy: factory.multi(
                "/dns/nameserver-policy",
                config_mapping(config, "/dns/nameserver-policy"),
                zenclash_i18n::text("config_inputs.placeholders.dns_mapping"),
            ),
            hosts: factory.multi(
                "/hosts",
                config_mapping(config, "/hosts"),
                zenclash_i18n::text("config_inputs.placeholders.address_mapping"),
            ),
            source: config_source(config, &["dns", "hosts"]),
        }
    }

    pub fn patch(&self, cx: &gpui_kit::App) -> Result<Value, String> {
        let enhanced_mode = text(&self.enhanced_mode, cx);
        if !enhanced_mode.is_empty()
            && !matches!(enhanced_mode.as_str(), "fake-ip" | "redir-host" | "normal")
        {
            return Err(zenclash_i18n::text("config_inputs.errors.dns_mode"));
        }
        let filter_mode = text(&self.fake_ip_filter_mode, cx);
        if !filter_mode.is_empty()
            && !matches!(filter_mode.as_str(), "blacklist" | "whitelist" | "rule")
        {
            return Err(zenclash_i18n::text("config_inputs.errors.fake_ip_mode"));
        }
        let mut dns = Map::new();
        insert_optional_string(
            &mut dns,
            "enhanced-mode",
            enhanced_mode,
            &self.source,
            "/dns/enhanced-mode",
        );
        insert_optional_string(
            &mut dns,
            "fake-ip-range",
            text(&self.fake_ip_range, cx),
            &self.source,
            "/dns/fake-ip-range",
        );
        insert_optional_string(
            &mut dns,
            "fake-ip-filter-mode",
            filter_mode,
            &self.source,
            "/dns/fake-ip-filter-mode",
        );
        insert_optional_lines(
            &mut dns,
            "fake-ip-filter",
            &self.fake_ip_filter,
            cx,
            &self.source,
            "/dns/fake-ip-filter",
        );
        insert_optional_lines(
            &mut dns,
            "default-nameserver",
            &self.default_nameserver,
            cx,
            &self.source,
            "/dns/default-nameserver",
        );
        insert_optional_lines(
            &mut dns,
            "nameserver",
            &self.nameserver,
            cx,
            &self.source,
            "/dns/nameserver",
        );
        insert_optional_lines(
            &mut dns,
            "proxy-server-nameserver",
            &self.proxy_server_nameserver,
            cx,
            &self.source,
            "/dns/proxy-server-nameserver",
        );
        insert_optional_lines(
            &mut dns,
            "direct-nameserver",
            &self.direct_nameserver,
            cx,
            &self.source,
            "/dns/direct-nameserver",
        );
        insert_optional_lines(
            &mut dns,
            "fallback",
            &self.fallback,
            cx,
            &self.source,
            "/dns/fallback",
        );
        let mut fallback_filter = Map::new();
        let geoip = text(&self.fallback_geoip, cx);
        if !geoip.is_empty() {
            let geoip = geoip
                .parse::<bool>()
                .map_err(|_| zenclash_i18n::text("settings_redesign.invalid_bool"))?;
            fallback_filter.insert("geoip".into(), Value::Bool(geoip));
        } else if self
            .source
            .pointer("/dns/fallback-filter/geoip")
            .is_some_and(|value| !value.is_null())
        {
            fallback_filter.insert("geoip".into(), Value::Null);
        }
        insert_optional_string(
            &mut fallback_filter,
            "geoip-code",
            text(&self.fallback_geoip_code, cx),
            &self.source,
            "/dns/fallback-filter/geoip-code",
        );
        insert_optional_lines(
            &mut fallback_filter,
            "ipcidr",
            &self.fallback_ipcidr,
            cx,
            &self.source,
            "/dns/fallback-filter/ipcidr",
        );
        insert_optional_lines(
            &mut fallback_filter,
            "domain",
            &self.fallback_domain,
            cx,
            &self.source,
            "/dns/fallback-filter/domain",
        );
        if !fallback_filter.is_empty() {
            dns.insert("fallback-filter".into(), Value::Object(fallback_filter));
        }
        insert_optional_mapping(
            &mut dns,
            "nameserver-policy",
            &text(&self.nameserver_policy, cx),
            "Nameserver Policy",
            &self.source,
            "/dns/nameserver-policy",
        )?;
        let mut patch = Map::new();
        if !dns.is_empty() {
            patch.insert("dns".into(), Value::Object(dns));
        }
        insert_optional_mapping(
            &mut patch,
            "hosts",
            &text(&self.hosts, cx),
            "Hosts",
            &self.source,
            "/hosts",
        )?;
        Ok(Value::Object(patch))
    }
}

impl SnifferInputs {
    pub(super) fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
        Self {
            http_ports: factory.single(
                "/sniffer/sniff/HTTP/ports",
                config_list_csv(config, "/sniffer/sniff/HTTP/ports"),
                "80, 8080-8880",
            ),
            tls_ports: factory.single(
                "/sniffer/sniff/TLS/ports",
                config_list_csv(config, "/sniffer/sniff/TLS/ports"),
                "443, 8443",
            ),
            quic_ports: factory.single(
                "/sniffer/sniff/QUIC/ports",
                config_list_csv(config, "/sniffer/sniff/QUIC/ports"),
                "443, 8443",
            ),
            skip_domain: factory.multi(
                "/sniffer/skip-domain",
                config_lines(config, "/sniffer/skip-domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain"),
            ),
            force_domain: factory.multi(
                "/sniffer/force-domain",
                config_lines(config, "/sniffer/force-domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain"),
            ),
            skip_dst_address: factory.multi(
                "/sniffer/skip-dst-address",
                config_lines(config, "/sniffer/skip-dst-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_address"),
            ),
            skip_src_address: factory.multi(
                "/sniffer/skip-src-address",
                config_lines(config, "/sniffer/skip-src-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_address"),
            ),
            source: config_source(config, &["sniffer"]),
        }
    }

    pub fn patch(&self, cx: &gpui_kit::App) -> Value {
        let mut sniff = Map::new();
        for (key, input, pointer) in [
            ("HTTP", &self.http_ports, "/sniffer/sniff/HTTP/ports"),
            ("TLS", &self.tls_ports, "/sniffer/sniff/TLS/ports"),
            ("QUIC", &self.quic_ports, "/sniffer/sniff/QUIC/ports"),
        ] {
            let values = ports(input, cx);
            if !values.is_empty() || self.source.pointer(pointer).is_some() {
                sniff.insert(
                    key.into(),
                    Value::Object(Map::from_iter([("ports".into(), Value::Array(values))])),
                );
            }
        }
        let mut sniffer = Map::new();
        if !sniff.is_empty() {
            sniffer.insert("sniff".into(), Value::Object(sniff));
        }
        insert_optional_lines(
            &mut sniffer,
            "skip-domain",
            &self.skip_domain,
            cx,
            &self.source,
            "/sniffer/skip-domain",
        );
        insert_optional_lines(
            &mut sniffer,
            "force-domain",
            &self.force_domain,
            cx,
            &self.source,
            "/sniffer/force-domain",
        );
        insert_optional_lines(
            &mut sniffer,
            "skip-dst-address",
            &self.skip_dst_address,
            cx,
            &self.source,
            "/sniffer/skip-dst-address",
        );
        insert_optional_lines(
            &mut sniffer,
            "skip-src-address",
            &self.skip_src_address,
            cx,
            &self.source,
            "/sniffer/skip-src-address",
        );
        if sniffer.is_empty() {
            Value::Object(Map::new())
        } else {
            Value::Object(Map::from_iter([("sniffer".into(), Value::Object(sniffer))]))
        }
    }
}

impl TunInputs {
    pub(super) fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
        Self {
            stack: factory.single(
                "/tun/stack",
                config_string(config, "/tun/stack", "gvisor"),
                "gvisor / mixed / system",
            ),
            device: factory.single(
                "/tun/device",
                config_string(config, "/tun/device", ""),
                zenclash_i18n::text("config_inputs.placeholders.tun_device"),
            ),
            mtu: factory.single(
                "/tun/mtu",
                config_number_or_empty(config, "/tun/mtu"),
                zenclash_i18n::text("config_inputs.placeholders.default_mtu"),
            ),
            dns_hijack: factory.single(
                "/tun/dns-hijack",
                config_list_csv(config, "/tun/dns-hijack"),
                "any:53, tcp://any:53",
            ),
            route_include_address: factory.multi(
                "/tun/route-address",
                config_lines(config, "/tun/route-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            route_exclude_address: factory.multi(
                "/tun/route-exclude-address",
                config_lines(config, "/tun/route-exclude-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            source: config_source(config, &["tun"]),
        }
    }

    pub fn patch(&self, cx: &gpui_kit::App) -> Result<Value, String> {
        let stack = text(&self.stack, cx);
        if !stack.is_empty() && !matches!(stack.as_str(), "gvisor" | "mixed" | "system") {
            return Err(zenclash_i18n::text("config_inputs.errors.tun_stack"));
        }
        let mtu_text = text(&self.mtu, cx);
        let mtu = if mtu_text.is_empty() {
            if self
                .source
                .pointer("/tun/mtu")
                .is_some_and(|value| !value.is_null())
            {
                Some(Value::Null)
            } else {
                None
            }
        } else {
            let mtu = mtu_text
                .parse::<u16>()
                .map_err(|_| zenclash_i18n::text("config_inputs.errors.mtu_integer"))?;
            if mtu == 0 {
                return Err(zenclash_i18n::text("config_inputs.errors.mtu_positive"));
            }
            Some(Value::from(mtu))
        };
        let mut tun = Map::new();
        insert_optional_string(&mut tun, "stack", stack, &self.source, "/tun/stack");
        insert_optional_string(
            &mut tun,
            "device",
            text(&self.device, cx),
            &self.source,
            "/tun/device",
        );
        if let Some(mtu) = mtu {
            tun.insert("mtu".into(), mtu);
        }
        let dns_hijack = csv(&self.dns_hijack, cx);
        if !dns_hijack.is_empty() || self.source.pointer("/tun/dns-hijack").is_some() {
            tun.insert(
                "dns-hijack".into(),
                Value::Array(dns_hijack.into_iter().map(Value::String).collect()),
            );
        }
        insert_optional_lines(
            &mut tun,
            "route-address",
            &self.route_include_address,
            cx,
            &self.source,
            "/tun/route-address",
        );
        insert_optional_lines(
            &mut tun,
            "route-exclude-address",
            &self.route_exclude_address,
            cx,
            &self.source,
            "/tun/route-exclude-address",
        );
        if tun.is_empty() {
            Ok(Value::Object(Map::new()))
        } else {
            Ok(Value::Object(Map::from_iter([(
                "tun".into(),
                Value::Object(tun),
            )])))
        }
    }
}
