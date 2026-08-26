use gpui::{AppContext, Context, Entity, Window};
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
}

pub(super) struct SnifferInputs {
    pub http_ports: Entity<InputState>,
    pub tls_ports: Entity<InputState>,
    pub quic_ports: Entity<InputState>,
    pub skip_domain: Entity<InputState>,
    pub force_domain: Entity<InputState>,
    pub skip_dst_address: Entity<InputState>,
    pub skip_src_address: Entity<InputState>,
}

pub(super) struct TunInputs {
    pub stack: Entity<InputState>,
    pub device: Entity<InputState>,
    pub mtu: Entity<InputState>,
    pub dns_hijack: Entity<InputState>,
    pub route_include_address: Entity<InputState>,
    pub route_exclude_address: Entity<InputState>,
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
        placeholder: &'static str,
    ) -> Entity<InputState> {
        input(value, placeholder, false, self.window, self.cx)
    }

    fn multi(&mut self, value: String, placeholder: &'static str) -> Entity<InputState> {
        input(value, placeholder, true, self.window, self.cx)
    }
}

impl DnsInputs {
    fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            enhanced_mode: factory.single(
                config_string(config, "/dns/enhanced-mode", "fake-ip"),
                "fake-ip / redir-host / normal",
            ),
            fake_ip_range: factory.single(
                config_string(config, "/dns/fake-ip-range", "198.18.0.1/16"),
                "198.18.0.1/16",
            ),
            fake_ip_filter_mode: factory.single(
                config_string(config, "/dns/fake-ip-filter-mode", "blacklist"),
                "blacklist / whitelist / rule",
            ),
            fake_ip_filter: factory.multi(
                config_lines(config, "/dns/fake-ip-filter"),
                "每行一个域名或规则",
            ),
            default_nameserver: factory.multi(
                config_lines(config, "/dns/default-nameserver"),
                "每行一个 IP DNS",
            ),
            nameserver: factory.multi(config_lines(config, "/dns/nameserver"), "每行一个 DNS 地址"),
            proxy_server_nameserver: factory.multi(
                config_lines(config, "/dns/proxy-server-nameserver"),
                "代理节点域名解析器",
            ),
            direct_nameserver: factory.multi(
                config_lines(config, "/dns/direct-nameserver"),
                "直连域名解析器",
            ),
            fallback: factory.multi(config_lines(config, "/dns/fallback"), "Fallback DNS"),
            fallback_geoip_code: factory.single(
                config_string(config, "/dns/fallback-filter/geoip-code", "CN"),
                "CN",
            ),
            fallback_ipcidr: factory.multi(
                config_lines(config, "/dns/fallback-filter/ipcidr"),
                "每行一个 CIDR",
            ),
            fallback_domain: factory.multi(
                config_lines(config, "/dns/fallback-filter/domain"),
                "每行一个域名规则",
            ),
            nameserver_policy: factory.multi(
                config_mapping(config, "/dns/nameserver-policy"),
                "domain: dns（YAML 映射）",
            ),
            hosts: factory.multi(
                config_mapping(config, "/hosts"),
                "domain: address（YAML 映射）",
            ),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
        let enhanced_mode = text(&self.enhanced_mode, cx);
        if !matches!(enhanced_mode.as_str(), "fake-ip" | "redir-host" | "normal") {
            return Err("DNS 增强模式必须是 fake-ip、redir-host 或 normal".into());
        }
        let filter_mode = text(&self.fake_ip_filter_mode, cx);
        if !matches!(filter_mode.as_str(), "blacklist" | "whitelist" | "rule") {
            return Err("Fake-IP 过滤模式必须是 blacklist、whitelist 或 rule".into());
        }
        let policy = yaml_mapping(&text(&self.nameserver_policy, cx), "Nameserver Policy")?;
        let hosts = yaml_mapping(&text(&self.hosts, cx), "Hosts")?;
        Ok(serde_json::json!({
            "dns": {
                "enhanced-mode": enhanced_mode,
                "fake-ip-range": text(&self.fake_ip_range, cx),
                "fake-ip-filter-mode": filter_mode,
                "fake-ip-filter": lines(&self.fake_ip_filter, cx),
                "default-nameserver": lines(&self.default_nameserver, cx),
                "nameserver": lines(&self.nameserver, cx),
                "proxy-server-nameserver": lines(&self.proxy_server_nameserver, cx),
                "direct-nameserver": lines(&self.direct_nameserver, cx),
                "fallback": lines(&self.fallback, cx),
                "fallback-filter": {
                    "geoip-code": text(&self.fallback_geoip_code, cx),
                    "ipcidr": lines(&self.fallback_ipcidr, cx),
                    "domain": lines(&self.fallback_domain, cx)
                },
                "nameserver-policy": policy
            },
            "hosts": hosts
        }))
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
            skip_domain: factory
                .multi(config_lines(config, "/sniffer/skip-domain"), "每行一个域名"),
            force_domain: factory.multi(
                config_lines(config, "/sniffer/force-domain"),
                "每行一个域名",
            ),
            skip_dst_address: factory.multi(
                config_lines(config, "/sniffer/skip-dst-address"),
                "每行一个 IP/CIDR",
            ),
            skip_src_address: factory.multi(
                config_lines(config, "/sniffer/skip-src-address"),
                "每行一个 IP/CIDR",
            ),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Value {
        serde_json::json!({"sniffer": {
            "sniff": {
                "HTTP": {"ports": ports(&self.http_ports, cx)},
                "TLS": {"ports": ports(&self.tls_ports, cx)},
                "QUIC": {"ports": ports(&self.quic_ports, cx)}
            },
            "skip-domain": lines(&self.skip_domain, cx),
            "force-domain": lines(&self.force_domain, cx),
            "skip-dst-address": lines(&self.skip_dst_address, cx),
            "skip-src-address": lines(&self.skip_src_address, cx)
        }})
    }
}

impl TunInputs {
    fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            stack: factory.single(
                config_string(config, "/tun/stack", "mixed"),
                "gvisor / mixed / system",
            ),
            device: factory.single(
                config_string(config, "/tun/device", "utun1500"),
                "TUN 设备名称",
            ),
            mtu: factory.single(config_number(config, "/tun/mtu", 1500), "1 - 65535"),
            dns_hijack: factory.single(
                config_list_csv(config, "/tun/dns-hijack"),
                "any:53, tcp://any:53",
            ),
            route_include_address: factory
                .multi(config_lines(config, "/tun/route-address"), "每行一个 CIDR"),
            route_exclude_address: factory.multi(
                config_lines(config, "/tun/route-exclude-address"),
                "每行一个 CIDR",
            ),
        }
    }

    pub fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
        let stack = text(&self.stack, cx);
        if !matches!(stack.as_str(), "gvisor" | "mixed" | "system") {
            return Err("TUN 网络栈必须是 gvisor、mixed 或 system".into());
        }
        let mtu = text(&self.mtu, cx)
            .parse::<u16>()
            .map_err(|_| "MTU 必须是 1 到 65535 的整数".to_owned())?;
        if mtu == 0 {
            return Err("MTU 必须大于 0".into());
        }
        Ok(serde_json::json!({"tun": {
            "stack": stack,
            "device": text(&self.device, cx),
            "mtu": mtu,
            "dns-hijack": csv(&self.dns_hijack, cx),
            "route-address": lines(&self.route_include_address, cx),
            "route-exclude-address": lines(&self.route_exclude_address, cx)
        }}))
    }
}

fn input(
    value: String,
    placeholder: &'static str,
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
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(value).map_err(|error| format!("{label} YAML 无效：{error}"))?;
    let json = serde_json::to_value(yaml).map_err(|error| format!("{label} 无法转换：{error}"))?;
    if json.is_object() {
        Ok(json)
    } else {
        Err(format!("{label} 必须是 YAML 映射"))
    }
}

pub(super) fn config_string(config: &Value, pointer: &str, default: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

pub(super) fn config_number(config: &Value, pointer: &str, default: u64) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .to_string()
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
