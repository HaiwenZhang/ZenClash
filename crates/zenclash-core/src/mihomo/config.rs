//! Stateless Mihomo configuration diffing, merging, validation, listener fallback, and synthesis pipeline.

pub(crate) mod diff;
pub(crate) mod listener_fallback;
pub(crate) mod merge;
pub(crate) mod pipeline;
pub(crate) mod tun;
pub(crate) mod validation;

pub use diff::{
    ConfigDiffEntry, ConfigDiffError, ConfigDiffKind, ConfigDiffReport, diff_yaml_configs,
};
pub use listener_fallback::ListenerPortFallback;
pub use merge::merge_profile_overrides;
pub use pipeline::{
    EffectiveConfigArtifact, EffectiveConfigPipeline, EffectiveConfigRequest,
    FULL_RELOAD_CONFIG_KEYS, HOT_UPDATE_CONFIG_KEYS, OverrideKeys, apply_controlled_override,
    yaml_content_hash,
};
pub use validation::{CoreConfigValidationError, CoreConfigValidator};
