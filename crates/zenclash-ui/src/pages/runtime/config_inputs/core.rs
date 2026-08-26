use gpui::Entity;
use gpui_component::input::InputState;
use serde_json::Value;

use super::{config_number, config_string, text, InputFactory};

pub(in crate::pages::runtime) struct CoreInputs {
    pub port: Entity<InputState>,
    pub socks_port: Entity<InputState>,
    pub mixed_port: Entity<InputState>,
    pub redir_port: Entity<InputState>,
    pub tproxy_port: Entity<InputState>,
    pub bind_address: Entity<InputState>,
    pub interface_name: Entity<InputState>,
    pub log_level: Entity<InputState>,
}

impl CoreInputs {
    pub(super) fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            port: factory.single(config_number(config, "/port", 0), "0 - 65535"),
            socks_port: factory.single(config_number(config, "/socks-port", 0), "0 - 65535"),
            mixed_port: factory.single(config_number(config, "/mixed-port", 7890), "0 - 65535"),
            redir_port: factory.single(config_number(config, "/redir-port", 0), "0 - 65535"),
            tproxy_port: factory.single(config_number(config, "/tproxy-port", 0), "0 - 65535"),
            bind_address: factory.single(
                config_string(config, "/bind-address", "*"),
                "* / 127.0.0.1 / 0.0.0.0",
            ),
            interface_name: factory.single(
                config_string(config, "/interface-name", ""),
                "留空自动选择出口接口",
            ),
            log_level: factory.single(
                config_string(config, "/log-level", "info"),
                "silent / error / warning / info / debug",
            ),
        }
    }

    pub(in crate::pages::runtime) fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
        let log_level = text(&self.log_level, cx).to_ascii_lowercase();
        if !matches!(
            log_level.as_str(),
            "silent" | "error" | "warning" | "info" | "debug"
        ) {
            return Err("日志等级必须是 silent、error、warning、info 或 debug".into());
        }
        let bind_address = text(&self.bind_address, cx);
        if bind_address.is_empty() {
            return Err("监听地址不能为空；仅本机监听可填写 127.0.0.1".into());
        }
        Ok(serde_json::json!({
            "port": parse_port(&text(&self.port, cx), "HTTP")?,
            "socks-port": parse_port(&text(&self.socks_port, cx), "SOCKS")?,
            "mixed-port": parse_port(&text(&self.mixed_port, cx), "Mixed")?,
            "redir-port": parse_port(&text(&self.redir_port, cx), "Redir")?,
            "tproxy-port": parse_port(&text(&self.tproxy_port, cx), "TPROXY")?,
            "bind-address": bind_address,
            "interface-name": text(&self.interface_name, cx),
            "log-level": log_level
        }))
    }
}

fn parse_port(value: &str, label: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("{label} 端口必须是 0 到 65535 的整数"))
}

#[cfg(test)]
mod tests {
    use super::parse_port;

    #[test]
    fn port_parser_accepts_disabled_and_maximum_ports() {
        assert_eq!(parse_port("0", "HTTP").unwrap(), 0);
        assert_eq!(parse_port("65535", "HTTP").unwrap(), 65_535);
    }

    #[test]
    fn port_parser_rejects_negative_and_overflow_values() {
        assert!(parse_port("-1", "HTTP").is_err());
        assert!(parse_port("65536", "HTTP").is_err());
    }
}
