//! Bounded structural differences between source and effective YAML configs.

use std::collections::HashSet;

use serde_yaml::Value;
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
    /// Source profile YAML could not be parsed or have merge keys expanded.
    #[error(
        "{}",
        zenclash_i18n::text_with("config_diff.errors.source", &[("error", .0.to_string())])
    )]
    Source(serde_yaml::Error),
    /// Effective YAML could not be parsed or have merge keys expanded.
    #[error(
        "{}",
        zenclash_i18n::text_with("config_diff.errors.effective", &[("error", .0.to_string())])
    )]
    Effective(serde_yaml::Error),
}

/// Recursively compares two YAML documents with deterministic ordering.
///
/// YAML `<<` merge keys are expanded on both sides prior to comparison.
/// Mappings are compared by key, while a changed sequence is emitted as one
/// bounded entry so large proxy/rule lists cannot flood the UI.
///
/// # Errors
///
/// Returns an error when either YAML document cannot be parsed or contains
/// invalid YAML merge-key targets.
pub fn diff_yaml_configs(
    source: &str,
    effective: &str,
    max_entries: usize,
) -> Result<ConfigDiffReport, ConfigDiffError> {
    let mut source: Value = serde_yaml::from_str(source).map_err(ConfigDiffError::Source)?;
    crate::mihomo::config::merge::expand_yaml_merges(&mut source)
        .map_err(ConfigDiffError::Source)?;
    let mut effective: Value =
        serde_yaml::from_str(effective).map_err(ConfigDiffError::Effective)?;
    crate::mihomo::config::merge::expand_yaml_merges(&mut effective)
        .map_err(ConfigDiffError::Effective)?;
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
    if let (Some(Value::Mapping(source)), Some(Value::Mapping(effective))) = (source, effective) {
        let mut seen = HashSet::with_capacity(source.len() + effective.len());
        let mut keys = Vec::with_capacity(source.len() + effective.len());
        for key in source.keys().chain(effective.keys()) {
            if seen.insert(key) {
                keys.push((key_display(key), key_kind_rank(key), key));
            }
        }
        keys.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        for (display, _, key) in keys {
            collect_differences(
                source.get(key),
                effective.get(key),
                child_path(&path, &display),
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

fn key_display(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => value_preview(other),
    }
}

const fn key_kind_rank(key: &Value) -> u8 {
    match key {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Sequence(_) => 4,
        Value::Mapping(_) => 5,
        Value::Tagged(_) => 6,
    }
}

fn child_path(parent: &str, key: &str) -> String {
    let key = key.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{key}")
}

fn value_preview(value: &Value) -> String {
    let mut writer = PreviewWriter::new(MAX_VALUE_PREVIEW_CHARS);
    writer.write_value(value);
    writer.finish()
}

struct PreviewWriter {
    output: String,
    chars: usize,
    limit: usize,
    truncated: bool,
}

impl PreviewWriter {
    fn new(limit: usize) -> Self {
        Self {
            output: String::new(),
            chars: 0,
            limit,
            truncated: false,
        }
    }

    fn push_str(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        for ch in text.chars() {
            if self.chars >= self.limit {
                self.truncated = true;
                return;
            }
            self.output.push(ch);
            self.chars += 1;
        }
    }

    fn push_char(&mut self, ch: char) {
        if self.truncated {
            return;
        }
        if self.chars >= self.limit {
            self.truncated = true;
            return;
        }
        self.output.push(ch);
        self.chars += 1;
    }

    fn write_quoted_string(&mut self, text: &str) {
        self.push_char('"');
        for ch in text.chars() {
            if self.truncated {
                return;
            }
            match ch {
                '"' => self.push_str("\\\""),
                '\\' => self.push_str("\\\\"),
                '\n' => self.push_str("\\n"),
                '\r' => self.push_str("\\r"),
                '\t' => self.push_str("\\t"),
                '\u{08}' => self.push_str("\\b"),
                '\u{0c}' => self.push_str("\\f"),
                c if (c as u32) < 0x20 => self.push_str(&format!("\\u{:04x}", c as u32)),
                c => self.push_char(c),
            }
        }
        self.push_char('"');
    }

    fn write_value(&mut self, value: &Value) {
        if self.truncated {
            return;
        }
        match value {
            Value::Null => self.push_str("null"),
            Value::Bool(true) => self.push_str("true"),
            Value::Bool(false) => self.push_str("false"),
            Value::Number(number) => self.push_str(&number.to_string()),
            Value::String(text) => self.write_quoted_string(text),
            Value::Sequence(items) => {
                self.push_char('[');
                for (index, item) in items.iter().enumerate() {
                    if self.truncated {
                        break;
                    }
                    if index > 0 {
                        self.push_char(',');
                    }
                    self.write_value(item);
                }
                self.push_char(']');
            }
            Value::Mapping(mapping) => {
                self.push_char('{');
                for (index, (key, item)) in mapping.iter().enumerate() {
                    if self.truncated {
                        break;
                    }
                    if index > 0 {
                        self.push_char(',');
                    }
                    self.write_value(key);
                    self.push_char(':');
                    self.write_value(item);
                }
                self.push_char('}');
            }
            Value::Tagged(tagged) => {
                self.push_str(&tagged.tag.to_string());
                self.push_char(' ');
                self.write_value(&tagged.value);
            }
        }
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.output.push('…');
        }
        self.output
    }
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

    #[test]
    fn yaml_merge_keys_are_expanded_before_diffing() {
        let source = "\
defaults: &defaults
  enable: false
  ipv6: false
dns:
  <<: *defaults
  enable: true
mode: rule
";
        let equivalent_effective = "\
defaults:
  enable: false
  ipv6: false
dns:
  enable: true
  ipv6: false
mode: rule
";
        let report = diff_yaml_configs(source, equivalent_effective, 20).unwrap();
        assert!(
            report.entries.is_empty(),
            "expanded merge keys matching effective must produce zero diffs: {:?}",
            report.entries
        );

        let modified_effective = "\
defaults:
  enable: false
  ipv6: false
dns:
  enable: true
  ipv6: true
mode: rule
";
        let report = diff_yaml_configs(source, modified_effective, 20).unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].path, "/dns/ipv6");
        assert_eq!(report.entries[0].kind, ConfigDiffKind::Changed);
        assert_eq!(report.entries[0].source.as_deref(), Some("false"));
        assert_eq!(report.entries[0].effective.as_deref(), Some("true"));
    }

    #[test]
    fn non_string_keys_and_special_floats_diff_without_json_conversion_errors() {
        let source = "\
123: alpha
true: enabled
nan_val: .nan
inf_val: .inf
";
        let effective = "\
123: beta
true: enabled
nan_val: .nan
inf_val: -.inf
";
        let report = diff_yaml_configs(source, effective, 20).unwrap();
        let summary = report
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.path.as_str(),
                    entry.kind,
                    entry.source.as_deref(),
                    entry.effective.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                (
                    "/123",
                    ConfigDiffKind::Changed,
                    Some("\"alpha\""),
                    Some("\"beta\"")
                ),
                (
                    "/inf_val",
                    ConfigDiffKind::Changed,
                    Some(".inf"),
                    Some("-.inf")
                ),
            ]
        );
    }
}
