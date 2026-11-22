//! JSON/YAML value extraction and patch-encoding helpers for configuration inputs.

use gpui_kit::Entity;
use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{InputState, TextareaState};
use serde_json::{Map, Number, Value};

pub(super) fn insert_optional_string(
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

pub(super) fn insert_optional_lines(
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

pub(super) fn insert_optional_mapping(
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

pub(crate) fn text<M: InputModeKind>(
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

pub(super) fn csv(input: &Entity<InputState>, cx: &gpui_kit::App) -> Vec<String> {
    text(input, cx)
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn ports(input: &Entity<InputState>, cx: &gpui_kit::App) -> Vec<Value> {
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

pub(crate) fn config_string(config: &Value, pointer: &str, default: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

pub(crate) fn config_number_or_empty(config: &Value, pointer: &str) -> String {
    config
        .pointer(pointer)
        .and_then(Value::as_u64)
        .map_or_else(String::new, |value| value.to_string())
}

pub(super) fn config_lines(config: &Value, pointer: &str) -> String {
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

pub(super) fn config_list_csv(config: &Value, pointer: &str) -> String {
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

pub(super) fn config_mapping(config: &Value, pointer: &str) -> String {
    let Some(value) = config.pointer(pointer).filter(|value| value.is_object()) else {
        return String::new();
    };
    serde_yaml::to_string(value)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
