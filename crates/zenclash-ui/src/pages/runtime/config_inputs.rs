use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{AnyInputState, InputState, TextareaState};
use gpui_kit::{AppContext, Context, Entity, Focusable, SharedString, Window};
use serde_json::{Map, Number, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[path = "config_inputs/core.rs"]
mod core;

pub(in crate::pages::runtime) use core::CoreInputs;

const CONFIG_INPUT_KEYS: &[&str] = &[
    "port",
    "socks-port",
    "mixed-port",
    "redir-port",
    "tproxy-port",
    "bind-address",
    "interface-name",
    "log-level",
    "geodata-mode",
    "geo-auto-update",
    "dns",
    "hosts",
    "sniffer",
    "tun",
];

pub(super) struct ConfigInputs {
    fields: HashMap<&'static str, InputField>,
    profile: Option<PathBuf>,
    reset: bool,
    pub core: CoreInputs,
    pub dns: DnsInputs,
    pub sniffer: SnifferInputs,
    pub tun: TunInputs,
}

pub(super) struct DnsInputs {
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

pub(super) struct SnifferInputs {
    pub http_ports: Entity<InputState>,
    pub tls_ports: Entity<InputState>,
    pub quic_ports: Entity<InputState>,
    pub skip_domain: Entity<TextareaState>,
    pub force_domain: Entity<TextareaState>,
    pub skip_dst_address: Entity<TextareaState>,
    pub skip_src_address: Entity<TextareaState>,
    source: Value,
}

pub(super) struct TunInputs {
    pub stack: Entity<InputState>,
    pub device: Entity<InputState>,
    pub mtu: Entity<InputState>,
    pub dns_hijack: Entity<InputState>,
    pub route_include_address: Entity<TextareaState>,
    pub route_exclude_address: Entity<TextareaState>,
    source: Value,
}

pub(super) struct SubmittedInputs {
    profile: Option<PathBuf>,
    values: Vec<(&'static str, String)>,
}

impl ConfigInputs {
    pub fn new(
        config: &Value,
        profile: Option<&Path>,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) -> Self {
        let mut fields = HashMap::new();
        let mut factory = InputFactory {
            window,
            cx,
            fields: &mut fields,
        };
        let core = CoreInputs::new(config, &mut factory);
        let dns = DnsInputs::new(config, &mut factory);
        let sniffer = SnifferInputs::new(config, &mut factory);
        let tun = TunInputs::new(config, &mut factory);
        Self {
            core,
            dns,
            sniffer,
            tun,
            fields,
            profile: profile.map(Path::to_path_buf),
            reset: false,
        }
    }

    pub(super) fn is_for_profile(&self, profile: Option<&Path>) -> bool {
        self.profile.as_deref() == profile
    }

    pub(super) fn submitted(&self, patch: &Value, cx: &gpui_kit::App) -> SubmittedInputs {
        SubmittedInputs {
            profile: self.profile.clone(),
            values: self
                .fields
                .iter()
                .filter(|(key, _)| patch.pointer(key).is_some())
                .map(|(&key, field)| (key, field.input.value(cx).to_string()))
                .collect(),
        }
    }

    pub(super) fn accept_submitted(&mut self, submitted: SubmittedInputs, cx: &gpui_kit::App) {
        if self.profile != submitted.profile {
            return;
        }
        for (key, value) in submitted.values {
            if let Some(field) = self.fields.get_mut(key) {
                field.baseline.accept(&field.input.value(cx), &value);
            }
        }
    }

    /// Discard only the fields owned by this page, preserving drafts on other tabs.
    pub(super) fn reset_page(
        &mut self,
        page: crate::pages::Page,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        use crate::pages::Page;
        for (&key, field) in &self.fields {
            let matches = match page {
                Page::Dns => key.starts_with("/dns/") || key == "/hosts",
                Page::Tun => key.starts_with("/tun/"),
                Page::Sniffer => key.starts_with("/sniffer/"),
                Page::SystemProxy => matches!(
                    key,
                    "/port" | "/mixed-port" | "/socks-port" | "/bind-address"
                ),
                _ => false,
            };
            if !matches {
                continue;
            }
            if let Some(input) = field.input.as_input() {
                input.update(cx, |input, cx| {
                    input.set_value(field.baseline.0.clone(), window, cx)
                });
            } else if let Some(input) = field.input.as_textarea() {
                input.update(cx, |input, cx| {
                    input.set_value(field.baseline.0.clone(), window, cx)
                });
            }
        }
    }

    pub(super) fn reset_on_next_refresh(&mut self) {
        self.reset = true;
    }

    pub(super) fn refresh(
        &mut self,
        config: &Value,
        profile: Option<&Path>,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        let reset = self.reset || self.profile.as_deref() != profile;
        let focused = reset
            .then(|| {
                self.fields.iter().find_map(|(&key, field)| {
                    field
                        .input
                        .focus_handle(cx)
                        .is_focused(window)
                        .then_some(key)
                })
            })
            .flatten();
        if reset {
            self.fields.clear();
        }
        let mut factory = InputFactory {
            window,
            cx,
            fields: &mut self.fields,
        };
        self.core = CoreInputs::new(config, &mut factory);
        self.dns = DnsInputs::new(config, &mut factory);
        self.sniffer = SnifferInputs::new(config, &mut factory);
        self.tun = TunInputs::new(config, &mut factory);
        if let Some(field) = focused.and_then(|key| self.fields.get(key)) {
            field.input.focus_handle(cx).focus(window, cx);
        }
        self.profile = profile.map(Path::to_path_buf);
        self.reset = false;
    }
}

#[derive(Default)]
struct FieldBaseline(String);

impl FieldBaseline {
    fn accept(&mut self, current: &str, submitted: &str) {
        if current == submitted {
            self.0 = submitted.to_owned();
        }
    }

    fn refresh(&mut self, current: &str, incoming: String) -> Option<String> {
        let replace = current != incoming && current == self.0;
        self.0 = incoming;
        replace.then(|| self.0.clone())
    }
}

struct InputField {
    input: AnyInputState,
    baseline: FieldBaseline,
    placeholder: SharedString,
}

pub(super) fn config_input_snapshot(config: Value) -> Value {
    config_source(&config, CONFIG_INPUT_KEYS)
}

pub(super) fn config_source(config: &Value, keys: &[&str]) -> Value {
    let Some(config) = config.as_object() else {
        return Value::Object(Map::new());
    };
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                config
                    .get(*key)
                    .cloned()
                    .map(|value| ((*key).to_owned(), value))
            })
            .collect(),
    )
}

pub(super) struct InputFactory<'a> {
    window: &'a mut Window,
    cx: &'a mut gpui_kit::App,
    fields: &'a mut HashMap<&'static str, InputField>,
}

impl InputFactory<'_> {
    pub(super) fn single(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: impl Into<SharedString>,
    ) -> Entity<InputState> {
        self.field(
            key,
            value,
            placeholder.into(),
            |state| state.as_input().cloned(),
            InputState::new,
        )
    }

    fn multi(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: impl Into<SharedString>,
    ) -> Entity<TextareaState> {
        self.field(
            key,
            value,
            placeholder.into(),
            |state| state.as_textarea().cloned(),
            |window, cx| TextareaState::new(window, cx).auto_grow(2, 7),
        )
    }

    fn field<M: InputModeKind>(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: SharedString,
        cached: impl FnOnce(&AnyInputState) -> Option<Entity<InputBaseState<M>>>,
        create: impl FnOnce(&mut Window, &mut Context<InputBaseState<M>>) -> InputBaseState<M>,
    ) -> Entity<InputBaseState<M>>
    where
        AnyInputState: From<Entity<InputBaseState<M>>>,
    {
        if let Some(field) = self.fields.get_mut(key)
            && let Some(input) = cached(&field.input)
        {
            let current = input.read(self.cx).value();
            if let Some(value) = field.baseline.refresh(&current, value) {
                input.update(self.cx, |input, cx| {
                    let focused = input.focus_handle(cx).is_focused(self.window);
                    let cursor = input.cursor_position();
                    input.set_value(value, self.window, cx);
                    if focused {
                        input.set_cursor_position(cursor, self.window, cx);
                    }
                });
            }
            if field.placeholder != placeholder {
                input.update(self.cx, |input, cx| {
                    input.set_placeholder(placeholder.clone(), self.window, cx)
                });
                field.placeholder = placeholder;
            }
            return input;
        }
        let input = self.cx.new(|cx| {
            create(self.window, cx)
                .placeholder(placeholder.clone())
                .default_value(value.clone())
        });
        self.fields.insert(
            key,
            InputField {
                input: input.clone().into(),
                baseline: FieldBaseline(value),
                placeholder,
            },
        );
        input
    }
}

impl DnsInputs {
    fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
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
    fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
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
    fn new(config: &Value, factory: &mut InputFactory<'_>) -> Self {
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
    input: &Entity<TextareaState>,
    cx: &gpui_kit::App,
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

pub(super) fn text<M: InputModeKind>(
    input: &Entity<InputBaseState<M>>,
    cx: &gpui_kit::App,
) -> String {
    input.read(cx).value().trim().to_owned()
}

fn lines(input: &Entity<TextareaState>, cx: &gpui_kit::App) -> Vec<String> {
    text(input, cx)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn csv(input: &Entity<InputState>, cx: &gpui_kit::App) -> Vec<String> {
    text(input, cx)
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn ports(input: &Entity<InputState>, cx: &gpui_kit::App) -> Vec<Value> {
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

#[cfg(test)]
#[path = "config_inputs/ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::FieldBaseline;
    use serde_json::json;

    use super::{config_input_snapshot, config_source};

    #[test]
    fn unchanged_refresh_leaves_text_and_selection_untouched() {
        let mut baseline = FieldBaseline("saved".into());
        assert_eq!(baseline.refresh("saved", "saved".into()), None);
    }

    #[test]
    fn background_refresh_preserves_edits_and_accepts_save_acknowledgements() {
        let mut baseline = FieldBaseline("original".into());
        assert_eq!(baseline.refresh("editing", "remote".into()), None);
        assert_eq!(baseline.refresh("editing", "editing".into()), None);
        assert_eq!(
            baseline.refresh("editing", "next remote".into()),
            Some("next remote".into())
        );
    }

    #[test]
    fn typing_after_submit_survives_the_older_save_readback() {
        let mut baseline = FieldBaseline("original".into());
        assert_eq!(baseline.refresh("newer typing", "submitted".into()), None);
        assert_eq!(baseline.refresh("newer typing", "submitted".into()), None);
    }

    #[test]
    fn acknowledged_input_is_normalized_without_discarding_later_typing() {
        let mut baseline = FieldBaseline("old".into());
        baseline.accept("  saved  ", "  saved  ");
        assert_eq!(
            baseline.refresh("  saved  ", "saved".into()),
            Some("saved".into())
        );
        baseline.accept("new typing", "submitted");
        assert_eq!(baseline.refresh("new typing", "submitted".into()), None);
    }

    #[test]
    fn input_snapshot_discards_large_runtime_only_sections() {
        let snapshot = config_input_snapshot(json!({
            "mixed-port": 7890,
            "geodata-mode": false,
            "geo-auto-update": true,
            "dns": { "enable": false, "nameserver": ["1.1.1.1"] },
            "rules": ["DOMAIN-SUFFIX,example.com,DIRECT"],
            "proxies": [{ "name": "large-runtime-section" }]
        }));

        assert_eq!(snapshot.pointer("/mixed-port"), Some(&json!(7890)));
        assert_eq!(
            snapshot.pointer("/dns/nameserver/0"),
            Some(&json!("1.1.1.1"))
        );
        assert_eq!(snapshot.pointer("/dns/enable"), Some(&json!(false)));
        assert_eq!(snapshot.get("geodata-mode"), Some(&json!(false)));
        assert_eq!(snapshot.get("geo-auto-update"), Some(&json!(true)));
        assert!(snapshot.get("rules").is_none());
        assert!(snapshot.get("proxies").is_none());
    }

    #[test]
    fn section_source_keeps_only_fields_used_by_its_patch() {
        let config = json!({
            "dns": { "enable": true },
            "hosts": { "router.local": "192.0.2.1" },
            "tun": { "enable": true }
        });

        assert_eq!(
            config_source(&config, &["dns", "hosts"]),
            json!({
                "dns": { "enable": true },
                "hosts": { "router.local": "192.0.2.1" }
            })
        );
    }
}
