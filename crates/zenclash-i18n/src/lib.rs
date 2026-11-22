//! Shared compile-time translations for ZenClash's native UI and platform surfaces.

// Keep Cargo's non-test dependency graph aware of locale-only edits so
// incremental release builds regenerate rust-i18n's embedded catalog.
const _: &str = include_str!("../locales/app.yml");
const _: &str = include_str!("../locales/ai_network.yml");

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
    fn macos_app_does_not_request_location_access() {
        assert!(
            !include_str!("../../../platforms/macos/Info.plist")
                .contains("NSLocationWhenInUseUsageDescription")
        );
    }

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
        assert_eq!(text("network.diagnostics.title"), "连接诊断");
        assert_eq!(
            text_for(EN, "resources.providers.healthcheck"),
            "Health Check"
        );
    }

    #[test]
    fn desktop_dashboard_labels_resolve_in_both_locales() {
        for (key, chinese, english) in [
            ("runtime.design.workspace", "工作台", "Workspace"),
            (
                "runtime.charts.top_five",
                "当前快照 · 前 5 类",
                "Current snapshot · top 5",
            ),
            (
                "tun.runtime.title",
                "TUN 运行时验证",
                "TUN Runtime Verification",
            ),
            (
                "connections.details.title",
                "连接详情",
                "Connection details",
            ),
            (
                "rules.charts.hits",
                "命中策略分布",
                "Matched policy distribution",
            ),
            ("logs.actions.inspect", "查看日志详情", "Inspect log"),
            ("logs.levels.all", "全部", "All"),
            ("profiles.design.remaining", "剩余", "Remaining"),
            (
                "proxies.design.latency_history",
                "延迟历史",
                "Latency history",
            ),
        ] {
            assert_eq!(text_for(ZH_CN, key), chinese);
            assert_eq!(text_for(EN, key), english);
        }
    }

    #[test]
    fn every_translation_leaf_contains_chinese_and_english_with_matching_placeholders() {
        let locales_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut files = std::fs::read_dir(&locales_dir)
            .expect("locale directory should be readable")
            .map(|entry| entry.expect("locale entry should be readable").path())
            .filter(|path| path.extension() == Some(std::ffi::OsStr::new("yml")))
            .collect::<Vec<_>>();
        files.sort();
        assert!(
            files
                .iter()
                .any(|path| path.file_name() == Some(std::ffi::OsStr::new("app.yml"))),
            "expected app.yml in locales/"
        );
        assert!(
            files
                .iter()
                .any(|path| path.file_name() == Some(std::ffi::OsStr::new("ai_network.yml"))),
            "expected ai_network.yml in locales/"
        );

        let mut total_leaves = 0usize;
        for path in files {
            let file_name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("<unknown>");
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {file_name}: {error}"));
            let document: Value = serde_yaml::from_str(&content).unwrap_or_else(|error| {
                panic!("translation YAML {file_name} should parse: {error}")
            });
            let leaves = validate_translation_tree(&document, file_name, "");
            assert!(
                leaves > 0,
                "locale file {file_name} contains no translation leaves"
            );
            total_leaves += leaves;
        }
        assert!(total_leaves > 0, "expected at least one translation leaf");
    }

    fn placeholders(template: &str) -> std::collections::BTreeSet<&str> {
        template
            .split("%{")
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .collect()
    }

    fn validate_translation_tree(value: &Value, file_name: &str, path: &str) -> usize {
        let Value::Mapping(mapping) = value else {
            panic!("translation node {file_name}:{path} should be a mapping");
        };
        let zh_key = Value::String(ZH_CN.into());
        let en_key = Value::String(EN.into());
        let is_leaf = mapping.get(&zh_key).is_some_and(Value::is_string)
            || mapping.get(&en_key).is_some_and(Value::is_string);
        if is_leaf {
            let zh = mapping
                .get(&zh_key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("translation {file_name}:{path} is missing {ZH_CN}"));
            let en = mapping
                .get(&en_key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("translation {file_name}:{path} is missing {EN}"));
            assert!(
                !zh.trim().is_empty(),
                "translation {file_name}:{path}.{ZH_CN} is empty"
            );
            assert!(
                !en.trim().is_empty(),
                "translation {file_name}:{path}.{EN} is empty"
            );
            assert_eq!(
                placeholders(zh),
                placeholders(en),
                "placeholder mismatch in {file_name}:{path}"
            );
            return 1;
        }

        let mut leaves = 0usize;
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
            leaves += validate_translation_tree(child, file_name, &child_path);
        }
        assert!(
            leaves > 0,
            "translation branch {file_name}:{path} contains no leaves"
        );
        leaves
    }
}
