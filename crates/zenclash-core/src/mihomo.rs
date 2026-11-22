//! Layer 1: Mihomo protocol, typed models, stateless config pipeline, and TUN/ruleset utilities.
//!
//! Groups controller endpoints, typed error enums, single-`Value` effective
//! configuration synthesis, YAML diff/merge/validation, read-only TUN runtime
//! observation, TUN permission status, ruleset conversion, proxy/system-proxy/network
//! value types, and controller response models.

pub(crate) mod config;
pub(crate) mod endpoint;
pub(crate) mod errors;
pub(crate) mod http_body;
pub(crate) mod models;
pub(crate) mod network_types;
pub(crate) mod proxy;
pub(crate) mod ruleset;
pub(crate) mod system_proxy_types;
pub(crate) mod tun;
pub(crate) mod websocket;

pub use crate::platform::core_kind::{CoreCapabilities, CoreKind, ParseCoreKindError};
pub use config::{
    ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport, CoreConfigValidationError,
    CoreConfigValidator, EffectiveConfigArtifact, EffectiveConfigPipeline, EffectiveConfigRequest,
    FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS, ListenerPortFallback, OverrideKeys,
    apply_controlled_override, diff_yaml_configs, merge_profile_overrides, yaml_content_hash,
};
pub use endpoint::MihomoEndpoint;
pub use errors::{
    AppPreferencesError, AppPreferencesResult, ControlledConfigError, ControlledConfigResult,
    MihomoError, MihomoResult, NativeHttpError, NativePropertyListError, PacError,
    PacServerOperation, ServiceCallError, ServiceStartupRejection, SystemProxyBackendKind,
    SystemProxyBackendUnavailable, SystemProxyError, SystemProxySessionError,
    SystemProxySessionResult, SystemProxySetting, SystemProxyTransactionCause,
    SystemProxyValidationError, VersionInfo,
};
pub use models::{
    Connection, ConnectionMetadata, ConnectionsSnapshot, ConnectionsSummary, DnsAnswer,
    DnsQueryResponse, DnsQuestion, DnsRecordType, MemorySnapshot, Provider, ProviderCatalog, Rule,
    RuleCatalog, RuleRuntimeStats, RuntimeConfig, SnifferConfig, TrafficAccountingConnection,
    TrafficAccountingMetadata, TrafficAccountingSnapshot, TunConfig,
};
pub use network_types::{
    DEFAULT_NETWORK_LATENCY_TARGETS, NetworkLatencyTarget, NetworkProbeError, NetworkProbeResult,
    PublicIpProvider,
};
pub use proxy::{
    DelayHistory, DelayResult, ProxyCatalog, ProxyGroup, ProxyGroupBehavior,
    ProxyGroupSelectionObservation, ProxyNode, ProxyNodeId,
};
pub use ruleset::{
    RulesetBehavior, RulesetConversion, RulesetConversionError, RulesetConversionResult,
    RulesetConverter,
};
pub use system_proxy_types::{
    SystemProxyMode, SystemProxyOwnership, default_pac_script, default_system_proxy_bypass,
    normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
};
pub use tun::{
    TunPermissionError, TunPermissionResult, TunPermissionStatus, TunRuntimeObservation,
    TunRuntimeObserver,
};
