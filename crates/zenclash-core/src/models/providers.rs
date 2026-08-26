use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Named proxy or rule providers returned by Mihomo.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderCatalog {
    /// Providers keyed by their configuration name.
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
}

/// Runtime metadata for one proxy or rule provider.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Provider {
    /// Provider name returned by Mihomo.
    #[serde(default)]
    pub name: String,
    /// Provider behavior type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Backing vehicle, such as HTTP or file.
    #[serde(default, rename = "vehicleType")]
    pub vehicle_type: String,
    /// Mihomo-provided last-update timestamp.
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
    /// URL used for provider health checks.
    #[serde(default, rename = "testUrl")]
    pub test_url: String,
    /// Raw provider proxy entries.
    #[serde(default)]
    pub proxies: Vec<Value>,
    /// Number of rules in a rule provider.
    #[serde(default, rename = "ruleCount")]
    pub rule_count: usize,
    /// Rule-provider behavior such as `domain`, `ipcidr`, or `classical`.
    #[serde(default)]
    pub behavior: String,
    /// Rule-provider storage format such as `yaml`, `text`, or `mrs`.
    #[serde(default)]
    pub format: String,
    /// Provider fields not yet modeled by `ZenClash`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Memory usage returned by Mihomo's `/memory` stream or endpoint.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Bytes currently in use.
    #[serde(default)]
    pub inuse: u64,
    /// Memory limit reported by the operating system.
    #[serde(default)]
    pub oslimit: u64,
}
