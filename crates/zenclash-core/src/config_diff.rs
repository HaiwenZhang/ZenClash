//! Bounded structural differences between source and effective YAML configs.

use std::collections::BTreeSet;

use serde_json::Value;
use thiserror::Error;

const MAX_VALUE_PREVIEW_CHARS: usize = 240;

/// How an effective configuration differs from its source profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigDiffKind {
    /// The path exists only in the effective configuration.
    Added,
    /// The path exists only in the source configuration.
    Removed,
    /// Both configurations contain the path with different values.
    Changed,
}

/// One deterministic JSON-Pointer-style configuration difference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigDiffEntry {
    /// Escaped path such as `/dns/enable`.
    pub path: String,
    /// Added, removed, or changed classification.
    pub kind: ConfigDiffKind,
    /// Compact source value when present.
    pub source: Option<String>,
    /// Compact effective value when present.
    pub effective: Option<String>,
}

/// Bounded result suitable for a native configuration inspector.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigDiffReport {
    /// Differences in deterministic path order.
    pub entries: Vec<ConfigDiffEntry>,
    /// Whether more differences existed after `max_entries` was reached.
    pub truncated: bool,
}

/// Errors produced while parsing either side of a YAML comparison.
#[derive(Debug, Error)]
pub enum ConfigDiffError {
    /// Source profile YAML could not be represented as JSON-like data.
    #[error("原始 Profile YAML 无法解析：{0}")]
    Source(serde_yaml::Error),
    /// Effective YAML could not be represented as JSON-like data.
    #[error("最终运行 YAML 无法解析：{0}")]
    Effective(serde_yaml::Error),
}

/// Recursively compares two YAML documents with deterministic ordering.
///
/// Objects are compared by key, while a changed array is emitted as one
/// bounded entry so large proxy/rule lists cannot flood the UI.
///
/// # Errors
///
/// Returns an error when either YAML document cannot be parsed into the
/// string-keyed JSON-compatible structure used by Mihomo configuration.
pub fn diff_yaml_configs(
    source: &str,
    effective: &str,
    max_entries: usize,
) -> Result<ConfigDiffReport, ConfigDiffError> {
    let source: Value = serde_yaml::from_str(source).map_err(ConfigDiffError::Source)?;
    let effective: Value = serde_yaml::from_str(effective).map_err(ConfigDiffError::Effective)?;
    let mut report = ConfigDiffReport::default();
    collect_differences(
        Some(&source),
        Some(&effective),
        String::new(),
        max_entries,
        &mut report,
    );
    Ok(report)
}

fn collect_differences(
    source: Option<&Value>,
    effective: Option<&Value>,
    path: String,
    max_entries: usize,
    report: &mut ConfigDiffReport,
) {
    if source == effective {
        return;
    }
    if report.entries.len() >= max_entries {
        report.truncated = true;
        return;
    }
    if let (Some(Value::Object(source)), Some(Value::Object(effective))) = (source, effective) {
        let keys = source
            .keys()
            .chain(effective.keys())
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for key in keys {
            collect_differences(
                source.get(key),
                effective.get(key),
                child_path(&path, key),
                max_entries,
                report,
            );
        }
        return;
    }
    report.entries.push(ConfigDiffEntry {
        path: if path.is_empty() { "/".into() } else { path },
        kind: match (source, effective) {
            (None, Some(_)) => ConfigDiffKind::Added,
            (Some(_), None) => ConfigDiffKind::Removed,
            _ => ConfigDiffKind::Changed,
        },
        source: source.map(value_preview),
        effective: effective.map(value_preview),
    });
}

fn child_path(parent: &str, key: &str) -> String {
    let key = key.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{key}")
}

fn value_preview(value: &Value) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| "<无法序列化>".into());
    if rendered.chars().count() <= MAX_VALUE_PREVIEW_CHARS {
        return rendered;
    }
    let mut preview = rendered
        .chars()
        .take(MAX_VALUE_PREVIEW_CHARS)
        .collect::<String>();
    preview.push('…');
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_diff_is_deterministic_and_uses_escaped_paths() {
        let report = diff_yaml_configs(
            "dns:\n  enable: false\n  old/key: yes\nmode: rule\n",
            "dns:\n  enable: true\n  new: value\nmode: rule\n",
            20,
        )
        .unwrap();

        let paths = report
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                ("/dns/enable", ConfigDiffKind::Changed),
                ("/dns/new", ConfigDiffKind::Added),
                ("/dns/old~1key", ConfigDiffKind::Removed),
            ]
        );
        assert!(!report.truncated);
    }

    #[test]
    fn arrays_are_one_bounded_change_and_entry_limit_is_reported() {
        let report = diff_yaml_configs(
            "rules: [A, B]\na: 1\nb: 2\n",
            "rules: [A, C]\na: 3\nb: 4\n",
            2,
        )
        .unwrap();

        assert_eq!(report.entries.len(), 2);
        assert!(report.truncated);
        assert_eq!(report.entries[0].path, "/a");
        assert_eq!(report.entries[1].path, "/b");
    }

    #[test]
    fn long_unicode_values_are_truncated_on_character_boundaries() {
        let source = format!("message: {}\n", "配".repeat(300));
        let report = diff_yaml_configs(&source, "message: changed\n", 10).unwrap();
        let preview = report.entries[0].source.as_deref().unwrap();

        assert!(preview.ends_with('…'));
        assert!(preview.is_char_boundary(preview.len()));
    }
}
