use serde::{Deserialize, Serialize};

/// Rule list returned by Mihomo's `/rules` endpoint.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuleCatalog {
    /// Rules in Mihomo evaluation order.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// One compiled Mihomo routing rule.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Rule {
    /// Rule matcher type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Matcher-specific rule payload.
    #[serde(default)]
    pub payload: String,
    /// Proxy group or policy selected by the rule.
    #[serde(default)]
    pub proxy: String,
    /// Provider-backed rule size, or Mihomo's sentinel value when unavailable.
    #[serde(default)]
    pub size: i64,
    /// Stable runtime index accepted by `/rules/disable` when supported.
    #[serde(default)]
    pub index: Option<usize>,
    /// Runtime hit counters and disabled state exposed by newer Mihomo builds.
    #[serde(default)]
    pub extra: Option<RuleRuntimeStats>,
}

/// Mutable runtime state and hit statistics attached to a compiled rule.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuleRuntimeStats {
    /// Whether Mihomo currently skips this rule during evaluation.
    #[serde(default)]
    pub disabled: bool,
    /// Number of rule evaluations that matched.
    #[serde(default)]
    pub hit_count: u64,
    /// Timestamp of the most recent match, as returned by Mihomo.
    #[serde(default)]
    pub hit_at: String,
    /// Number of rule evaluations that did not match.
    #[serde(default)]
    pub miss_count: u64,
    /// Timestamp of the most recent miss, as returned by Mihomo.
    #[serde(default)]
    pub miss_at: String,
}
