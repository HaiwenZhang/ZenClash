use gpui::Entity;
use gpui_component::input::InputState;
use serde_json::{Map, Value};

use super::{config_number_or_empty, config_string, text, InputFactory};

pub(in crate::pages::runtime) struct CoreInputs {
    pub port: Entity<InputState>,
    pub socks_port: Entity<InputState>,
    pub mixed_port: Entity<InputState>,
    pub redir_port: Entity<InputState>,
    pub tproxy_port: Entity<InputState>,
    pub bind_address: Entity<InputState>,
    pub interface_name: Entity<InputState>,
    pub log_level: Entity<InputState>,
    source: Value,
}

impl CoreInputs {
    pub(super) fn new(config: &Value, factory: &mut InputFactory<'_, '_>) -> Self {
        Self {
            port: factory.single(config_number_or_empty(config, "/port"), "0 - 65535"),
            socks_port: factory.single(config_number_or_empty(config, "/socks-port"), "0 - 65535"),
            mixed_port: factory.single(
                config_number_or_empty(config, "/mixed-port"),
                "默认 7890；0 表示关闭",
            ),
            redir_port: factory.single(config_number_or_empty(config, "/redir-port"), "0 - 65535"),
            tproxy_port: factory
                .single(config_number_or_empty(config, "/tproxy-port"), "0 - 65535"),
            bind_address: factory.single(
                config_string(config, "/bind-address", ""),
                "* / 127.0.0.1 / 0.0.0.0",
            ),
            interface_name: factory.single(
                config_string(config, "/interface-name", ""),
                "留空自动选择出口接口",
            ),
            log_level: factory.single(
                config_string(config, "/log-level", ""),
                "silent / error / warning / info / debug",
            ),
            source: config.clone(),
        }
    }

    pub(in crate::pages::runtime) fn patch(&self, cx: &gpui::App) -> Result<Value, String> {
        let log_level = text(&self.log_level, cx).to_ascii_lowercase();
        if !log_level.is_empty()
            && !matches!(
                log_level.as_str(),
                "silent" | "error" | "warning" | "info" | "debug"
            )
        {
            return Err("日志等级必须是 silent、error、warning、info 或 debug".into());
        }
        let bind_address = text(&self.bind_address, cx);
        if bind_address.is_empty() && self.source.pointer("/bind-address").is_some() {
            return Err("监听地址不能为空；仅本机监听可填写 127.0.0.1".into());
        }
        let mut patch = Map::new();
        insert_port(
            &mut patch,
            "port",
            &text(&self.port, cx),
            "HTTP",
            self.source.pointer("/port").is_some(),
        )?;
        insert_port(
            &mut patch,
            "socks-port",
            &text(&self.socks_port, cx),
            "SOCKS",
            self.source.pointer("/socks-port").is_some(),
        )?;
        insert_port(
            &mut patch,
            "mixed-port",
            &text(&self.mixed_port, cx),
            "Mixed",
            self.source.pointer("/mixed-port").is_some(),
        )?;
        insert_port(
            &mut patch,
            "redir-port",
            &text(&self.redir_port, cx),
            "Redir",
            self.source.pointer("/redir-port").is_some(),
        )?;
        insert_port(
            &mut patch,
            "tproxy-port",
            &text(&self.tproxy_port, cx),
            "TPROXY",
            self.source.pointer("/tproxy-port").is_some(),
        )?;
        insert_text(
            &mut patch,
            "bind-address",
            bind_address,
            self.source.pointer("/bind-address").is_some(),
        );
        insert_text(
            &mut patch,
            "interface-name",
            text(&self.interface_name, cx),
            self.source.pointer("/interface-name").is_some(),
        );
        insert_text(
            &mut patch,
            "log-level",
            log_level,
            self.source.pointer("/log-level").is_some(),
        );
        Ok(Value::Object(patch))
    }
}

fn insert_port(
    patch: &mut Map<String, Value>,
    key: &str,
    value: &str,
    label: &str,
    existed: bool,
) -> Result<(), String> {
    if value.is_empty() && !existed {
        return Ok(());
    }
    patch.insert(key.to_owned(), Value::from(parse_port(value, label)?));
    Ok(())
}

fn insert_text(patch: &mut Map<String, Value>, key: &str, value: String, existed: bool) {
    if !value.is_empty() || existed {
        patch.insert(key.to_owned(), Value::String(value));
    }
}

fn parse_port(value: &str, label: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("{label} 端口必须是 0 到 65535 的整数"))
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::{insert_port, parse_port};

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

    #[test]
    fn missing_blank_port_is_not_fabricated_but_existing_blank_is_rejected() {
        let mut patch = Map::new();
        insert_port(&mut patch, "mixed-port", "", "Mixed", false).unwrap();
        assert!(patch.is_empty());
        assert!(insert_port(&mut patch, "mixed-port", "", "Mixed", true).is_err());
    }
}
