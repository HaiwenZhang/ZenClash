use gpui::{AppContext, Context, Entity, SharedString, Window};
use gpui_component::input::InputState;
use serde_json::{Map, Number, Value};

mod core;

pub(in crate::pages::runtime) use core::CoreInputs;

pub(super) struct ConfigInputs {
    pub core: CoreInputs,
    pub dns: DnsInputs,
    pub sniffer: SnifferInputs,
    pub tun: TunInputs,
}

pub(super) struct DnsInputs {
    pub enhanced_mode: Entity<InputState>,
    pub fake_ip_range: Entity<InputState>,
    pub fake_ip_filter_mode: Entity<InputState>,
    pub fake_ip_filter: Entity<InputState>,
    pub default_nameserver: Entity<InputState>,
    pub nameserver: Entity<InputState>,
    pub proxy_server_nameserver: Entity<InputState>,
    pub direct_nameserver: Entity<InputState>,
    pub fallback: Entity<InputState>,
    pub fallback_geoip_code: Entity<InputState>,
    pub fallback_ipcidr: Entity<InputState>,
    pub fallback_domain: Entity<InputState>,
    pub nameserver_policy: Entity<InputState>,
    pub hosts: Entity<InputState>,
    source: Value,
}

pub(super) struct SnifferInputs {
    pub http_ports: Entity<InputState>,
    pub tls_ports: Entity<InputState>,
    pub quic_ports: Entity<InputState>,
    pub skip_domain: Entity<InputState>,
    pub force_domain: Entity<InputState>,
    pub skip_dst_address: Entity<InputState>,
    pub skip_src_address: Entity<InputState>,
    source: Value,
}

pub(super) struct TunInputs {
    pub stack: Entity<InputState>,
    pub device: Entity<InputState>,
    pub mtu: Entity<InputState>,
    pub dns_hijack: Entity<InputState>,
    pub route_include_address: Entity<InputState>,
    pub route_exclude_address: Entity<InputState>,
    source: Value,
}

impl ConfigInputs {
    pub fn new(config: &Value, window: &mut Window, cx: &mut Context<super::RuntimePage>) -> Self {
        let mut factory = InputFactory { window, cx };
        Self {
            core: CoreInputs::new(config, &mut factory),
            dns: DnsInputs::new(config, &mut factory),
            sniffer: SnifferInputs::new(config, &mut factory),
            tun: TunInputs::new(config, &mut factory),
        }
    }
}

pub(super) struct InputFactory<'a, 'b> {
    window: &'a mut Window,
    cx: &'a mut Context<'b, super::RuntimePage>,
}

impl InputFactory<'_, '_> {
    pub(super) fn single(
        &mut self,
        value: String,
        placeholder: impl Into<SharedString>,
    ) -> Entity<InputState> {
        input(value, placeholder.into(), false, self.window, self.cx)
    }

    fn multi(&mut self, value: String, placeholder: impl Into<SharedString>) -> Entity<InputState> {
        input(value, placeholder.into(), true, self.window, self.cx)
    }
}

impl DnsInputs {
    fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            enhanced_mode: factory.single(
                config_string(config, "/dns/enhanced-mode", ""),
                "fake-ip / redir-host / normal",
            ),
            fake_ip_range: factory.single(
                config_string(config, "/dns/fake-ip-range", ""),
                "198.18.0.1/16",
            ),
            fake_ip_filter_mode: factory.single(
                config_string(config, "/dns/fake-ip-filter-mode", ""),
                "blacklist / whitelist / rule",
            ),
            fake_ip_filter: factory.multi(
                config_lines(config, "/dns/fake-ip-filter"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain_or_rule"),
            ),
            default_nameserver: factory.multi(
                config_lines(config, "/dns/default-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.one_ip_dns"),
            ),
            nameserver: factory.multi(
                config_lines(config, "/dns/nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.one_dns"),
            ),
            proxy_server_nameserver: factory.multi(
                config_lines(config, "/dns/proxy-server-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.proxy_resolver"),
            ),
            direct_nameserver: factory.multi(
                config_lines(config, "/dns/direct-nameserver"),
                zenclash_i18n::text("config_inputs.placeholders.direct_resolver"),
            ),
            fallback: factory.multi(config_lines(config, "/dns/fallback"), "Fallback DNS"),
            fallback_geoip_code: factory.single(
                config_string(config, "/dns/fallback-filter/geoip-code", ""),
                "CN",
            ),
            fallback_ipcidr: factory.multi(
                config_lines(config, "/dns/fallback-filter/ipcidr"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            fallback_domain: factory.multi(
                config_lines(config, "/dns/fallback-filter/domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain_rule"),
            ),
            nameserver_policy: factory.multi(
                config_mapping(config, "/dns/nameserver-policy"),
                zenclash_i18n::text("config_inputs.placeholders.dns_mapping"),
            ),
            hosts: factory.multi(
                config_mapping(config, "/hosts"),
                zenclash_i18n::text("config_inputs.placeholders.address_mapping"),
            ),
            source: config.clone(),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
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
    fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            http_ports: factory.single(
                config_list_csv(config, "/sniffer/sniff/HTTP/ports"),
                "80, 8080-8880",
            ),
            tls_ports: factory.single(
                config_list_csv(config, "/sniffer/sniff/TLS/ports"),
                "443, 8443",
            ),
            quic_ports: factory.single(
                config_list_csv(config, "/sniffer/sniff/QUIC/ports"),
                "443, 8443",
            ),
            skip_domain: factory.multi(
                config_lines(config, "/sniffer/skip-domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain"),
            ),
            force_domain: factory.multi(
                config_lines(config, "/sniffer/force-domain"),
                zenclash_i18n::text("config_inputs.placeholders.one_domain"),
            ),
            skip_dst_address: factory.multi(
                config_lines(config, "/sniffer/skip-dst-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_address"),
            ),
            skip_src_address: factory.multi(
                config_lines(config, "/sniffer/skip-src-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_address"),
            ),
            source: config.clone(),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Value {
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
    fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            stack: factory.single(
                config_string(config, "/tun/stack", ""),
                "gvisor / mixed / system",
            ),
            device: factory.single(
                config_string(config, "/tun/device", ""),
                zenclash_i18n::text("config_inputs.placeholders.tun_device"),
            ),
            mtu: factory.single(
                config_number_or_empty(config, "/tun/mtu"),
                zenclash_i18n::text("config_inputs.placeholders.default_mtu"),
            ),
            dns_hijack: factory.single(
                config_list_csv(config, "/tun/dns-hijack"),
                "any:53, tcp://any:53",
            ),
            route_include_address: factory.multi(
                config_lines(config, "/tun/route-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            route_exclude_address: factory.multi(
                config_lines(config, "/tun/route-exclude-address"),
                zenclash_i18n::text("config_inputs.placeholders.one_cidr"),
            ),
            source: config.clone(),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
        let stack = text(&self.stack, cx);
        if !stack.is_empty() && !matches!(stack.as_str(), "gvisor" | "mixed" | "system") {
            return Err(zenclash_i18n::text("config_inputs.errors.tun_stack"));
        }
        let mtu_text = text(&self.mtu, cx);
        let mtu = if mtu_text.is_empty() && self.source.pointer("/tun/mtu").is_none() {
            None
        } else {
            let mtu = mtu_text
                .parse::<u16>()
                .map_err(|_| zenclash_i18n::text("config_inputs.errors.mtu_integer"))?;
            if mtu == 0 {
                return Err(zenclash_i18n::text("config_inputs.errors.mtu_positive"));
            }
            Some(mtu)
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
            tun.insert("mtu".into(), Value::from(mtu));
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

fn insert_optional_string(
    map: &mut Map<String, Value>,
    key: &str,
    value: String,
    source: &Value,
    pointer: &str,
) {
    if !value.is_empty() || source.pointer(pointer).is_some() {
        map.insert(key.into(), Value::String(value));
    }
}

fn insert_optional_lines(
    map: &mut Map<String, Value>,
    key: &str,
    input: &Entity<InputState>,
    cx: &gpui::App,
    source: &Value,
    pointer: &str,
) {
    let values = lines(input, cx);
    if !values.is_empty() || source.pointer(pointer).is_some() {
        map.insert(
            key.into(),
            Value::Array(values.into_iter().map(Value::String).collect()),
        );
    }
}

fn insert_optional_mapping(
    map: &mut Map<String, Value>,
    key: &str,
    text: &str,
    label: &str,
    source: &Value,
    pointer: &str,
) -> Result<(), String> {
    if !text.trim().is_empty() || source.pointer(pointer).is_some() {
        map.insert(key.into(), yaml_mapping(text, label)?);
    }
    Ok(())
}

fn input(
    value: String,
    placeholder: SharedString,
    multiline: bool,
    window: &mut Window,
    cx: &mut Context<super::RuntimePage>,
) -> Entity<InputState> {
    cx.new(|cx| {
        let state = InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value);
        if multiline {
            state.auto_grow(2, 7)
        } else {
            state
        }
    })
}

pub(super) fn text(input: &Entity<InputState>, cx: &gpui::App) -> String {
    input.read(cx).value().trim().to_owned()
}

fn lines(input: &Entity<InputState>, cx: &gpui::App) -> Vec<String> {
    text(input, cx)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn csv(input: &Entity<InputState>, cx: &gpui::App) -> Vec<String> {
    text(input, cx)
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn ports(input: &Entity<InputState>, cx: &gpui::App) -> Vec<Value> {
    csv(input, cx)
        .into_iter()
        .map(|port| {
            port.parse::<u16>().map_or(Value::String(port), |value| {
                Value::Number(Number::from(value))
            })
        })
        .collect()
}

fn yaml_mapping(value: &str, label: &str) -> Result<Value, String> {
    if value.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let yaml: serde_yaml::Value = serde_yaml::from_str(value).map_err(|error| {
        zenclash_i18n::text_with(
            "config_inputs.errors.invalid_yaml",
            &[("label", label.to_owned()), ("error", error.to_string())],
        )
    })?;
    let json = serde_json::to_value(yaml).map_err(|error| {
        zenclash_i18n::text_with(
            "config_inputs.errors.yaml_conversion",
            &[("label", label.to_owned()), ("error", error.to_string())],
        )
    })?;
    if json.is_object() {
        Ok(json)
    } else {
        Err(zenclash_i18n::text_with(
            "config_inputs.errors.yaml_mapping",
            &[("label", label.to_owned())],
        ))
    }
}

pub(super) fn config_string(config: &Value, pointer: &str, default: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

pub(super) fn config_number_or_empty(config: &Value, pointer: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_u64)
        .map_or_else(String::new, |value| value.to_string())
}

fn config_lines(config: &Value, pointer: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_array)
        .map_or_else(String::new, |items| {
            items
                .iter()
                .filter_map(scalar_text)
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn config_list_csv(config: &Value, pointer: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_array)
        .map_or_else(String::new, |items| {
            items
                .iter()
                .filter_map(scalar_text)
                .collect::<Vec<_>>()
                .join(", ")
        })
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn config_mapping(config: &Value, pointer: &str) -> String {
    let Some(value) = config.pointer(pointer).filter(|value| value.is_object()) else {
        return String::new();
    };
    serde_yaml::to_string(value)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
