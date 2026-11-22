use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use gpui_kit::Window;
use serde_json::{Map, Value};

mod codec;
mod core;
mod fields;
mod read_identity;
mod read_state;
mod sections;

pub(crate) use codec::{config_number_or_empty, config_string, text};
pub(crate) use core::CoreInputs;
pub(crate) use fields::InputFactory;
use fields::InputField;
pub(crate) use read_identity::ConfigInputsTaskToken;
pub(crate) use sections::{DnsInputs, SnifferInputs, TunInputs};

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

pub(crate) struct ConfigInputs {
    read: read_state::ConfigInputsReadState,
    fields: HashMap<&'static str, InputField>,
    profile: Option<PathBuf>,
    reset: bool,
    pub core: CoreInputs,
    pub dns: DnsInputs,
    pub sniffer: SnifferInputs,
    pub tun: TunInputs,
}

pub(crate) struct SubmittedInputs {
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
            read: read_state::ConfigInputsReadState::new(config.clone()),
            core,
            dns,
            sniffer,
            tun,
            fields,
            profile: profile.map(Path::to_path_buf),
            reset: false,
        }
    }

    pub(crate) fn is_for_profile(&self, profile: Option<&Path>) -> bool {
        self.profile.as_deref() == profile
    }

    pub(crate) fn effective(&self) -> &Value {
        self.read.effective()
    }

    pub(crate) fn read_generation(&self) -> u64 {
        self.read.generation()
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.read.is_loading()
    }

    pub(crate) fn begin_read(&mut self, profile: Option<&Path>) -> Option<ConfigInputsTaskToken> {
        self.read.begin(profile)
    }

    pub(crate) fn finish_read(
        &mut self,
        token: &ConfigInputsTaskToken,
        profile: Option<&Path>,
    ) -> bool {
        self.read.finish(token, profile)
    }

    pub(crate) fn invalidate_read(&mut self) {
        self.read.invalidate();
    }

    pub(crate) fn begin_bootstrap_read(&mut self) {
        self.read.begin_bootstrap();
    }

    pub(crate) fn finish_bootstrap_read(&mut self) {
        self.read.finish_bootstrap();
    }

    pub(crate) fn accept_read(
        &mut self,
        config: Value,
        token: ConfigInputsTaskToken,
        profile: Option<&Path>,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        self.refresh(&config, profile, window, cx);
        self.read.accept(config, token.profile);
    }

    pub(crate) fn record_read_failure(&mut self, token: ConfigInputsTaskToken) {
        self.read.record_failure(token.profile);
    }

    pub(crate) fn submitted(&self, patch: &Value, cx: &gpui_kit::App) -> SubmittedInputs {
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

    pub(crate) fn accept_submitted(&mut self, submitted: SubmittedInputs, cx: &gpui_kit::App) {
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
    pub(crate) fn reset_page(
        &mut self,
        page: crate::foundation::actions::Page,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        use crate::foundation::actions::Page;
        for (&key, field) in &self.fields {
            let matches = match page {
                Page::Dns => key.starts_with("/dns/") || key == "/hosts",
                Page::Tun => key.starts_with("/tun/"),
                Page::Sniffer => key.starts_with("/sniffer/"),
                Page::SystemProxy => matches!(
                    key,
                    "/port" | "/mixed-port" | "/socks-port" | "/bind-address"
                ),
                Page::Mihomo => matches!(
                    key,
                    "/port"
                        | "/mixed-port"
                        | "/socks-port"
                        | "/redir-port"
                        | "/tproxy-port"
                        | "/bind-address"
                        | "/interface-name"
                        | "/log-level"
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

    pub(crate) fn reset_on_next_refresh(&mut self) {
        self.reset = true;
    }

    pub(crate) fn refresh(
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

pub(crate) fn config_input_snapshot(config: Value) -> Value {
    config_source(&config, CONFIG_INPUT_KEYS)
}

pub(crate) fn config_source(config: &Value, keys: &[&str]) -> Value {
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_tests;
