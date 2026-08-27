//! Shared compile-time translations for ZenClash's native UI and platform surfaces.

rust_i18n::i18n!("locales", fallback = "zh-CN");

/// Simplified Chinese locale identifier used by the application preferences.
pub const ZH_CN: &str = "zh-CN";
/// English locale identifier used by the application preferences.
pub const EN: &str = "en";

/// Changes the process-wide locale used by subsequent translation lookups.
pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// Returns the active process-wide locale.
#[must_use]
pub fn locale() -> String {
    rust_i18n::locale().to_string()
}

/// Looks up a stable translation key and returns an owned string suitable for GPUI.
#[must_use]
pub fn text(key: &str) -> String {
    rust_i18n::t!(key).to_string()
}

/// Looks up a stable key for a specific supported locale without changing the
/// process-wide locale.
#[must_use]
pub fn text_for(locale: &str, key: &str) -> String {
    rust_i18n::t!(key, locale = locale).to_string()
}

/// Looks up a translation and substitutes named `%{name}` placeholders.
///
/// Keeping interpolation here gives native menus and GPUI elements the same
/// translation source while allowing the caller to retain control of domain values.
#[must_use]
pub fn text_with(key: &str, values: &[(&str, String)]) -> String {
    let mut translated = text(key);
    for (name, value) in values {
        translated = translated.replace(&format!("%{{{name}}}"), value);
    }
    translated
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value;

    #[test]
    fn supported_locales_switch_and_interpolate() {
        set_locale(EN);
        assert_eq!(text("common.actions.refresh"), "Refresh");
        assert_eq!(
            text_with(
                "common.count.visible_total",
                &[("visible", "3".into()), ("total", "8".into())],
            ),
            "3 of 8"
        );

        set_locale(ZH_CN);
        assert_eq!(text("common.actions.refresh"), "刷新");
        assert_eq!(
            text_with(
                "common.count.visible_total",
                &[("visible", "3".into()), ("total", "8".into())],
            ),
            "3 / 8 条"
        );
        assert_eq!(text_for(EN, "common.actions.refresh"), "Refresh");
        assert_eq!(text_for(ZH_CN, "common.actions.refresh"), "刷新");
    }

    #[test]
    fn every_translation_leaf_contains_chinese_and_english() {
        let document: Value = serde_yaml::from_str(include_str!("../locales/app.yml"))
            .expect("translation YAML should parse");
        validate_translation_tree(&document, "");
    }

    fn validate_translation_tree(value: &Value, path: &str) {
        let Value::Mapping(mapping) = value else {
            panic!("translation node {path} should be a mapping");
        };
        let zh_key = Value::String(ZH_CN.into());
        let en_key = Value::String(EN.into());
        if mapping.contains_key(&zh_key) {
            let zh = mapping
                .get(&zh_key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("translation {path} is missing {ZH_CN}"));
            let en = mapping
                .get(&en_key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("translation {path} is missing {EN}"));
            assert!(!zh.trim().is_empty(), "translation {path}.{ZH_CN} is empty");
            assert!(!en.trim().is_empty(), "translation {path}.{EN} is empty");
            return;
        }

        for (key, child) in mapping {
            let Some(key) = key.as_str() else {
                continue;
            };
            if key == "_version" {
                continue;
            }
            let child_path = if path.is_empty() {
                key.to_owned()
            } else {
                format!("{path}.{key}")
            };
            validate_translation_tree(child, &child_path);
        }
    }
}
