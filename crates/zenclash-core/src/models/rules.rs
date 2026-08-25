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
}
