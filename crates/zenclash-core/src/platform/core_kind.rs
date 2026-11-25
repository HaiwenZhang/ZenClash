//! Supported runtime identity and Mihomo API capability declarations.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Runtime core launched or attached to by `ZenClash`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoreKind {
    /// The Go Mihomo runtime with the complete controller feature set.
    #[default]
    Mihomo,
}

impl CoreKind {
    /// Human-readable core name shown in native UI and diagnostics.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mihomo => "Mihomo",
        }
    }

    /// Executable filename without a platform-specific extension.
    #[must_use]
    pub const fn executable_stem(self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
        }
    }

    /// Core-specific writable-home environment variable.
    #[must_use]
    pub const fn home_environment_variable(self) -> &'static str {
        match self {
            Self::Mihomo => "ZENCLASH_MIHOMO_HOME",
        }
    }

    /// Controller features implemented reliably by this core.
    #[must_use]
    pub const fn capabilities(self) -> CoreCapabilities {
        match self {
            Self::Mihomo => CoreCapabilities {
                config_validation: true,
                full_config_reload: true,
                rule_toggle: true,
                core_upgrade: true,
                geodata_update: true,
                external_ui_update: true,
                ruleset_conversion: true,
                udp_connection_tracking: true,
            },
        }
    }
}

impl fmt::Display for CoreKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

impl FromStr for CoreKind {
    type Err = ParseCoreKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mihomo" => Ok(Self::Mihomo),
            _ => Err(ParseCoreKindError(value.trim().to_owned())),
        }
    }
}

/// Error returned when a runtime-core identifier is not recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseCoreKindError(String);

impl fmt::Display for ParseCoreKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "不支持的内核 `{}`；可选值为 mihomo", self.0)
    }
}

impl std::error::Error for ParseCoreKindError {}

/// Controller and command-line features provided by Mihomo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreCapabilities {
    /// Whether the executable supports native `-t` configuration validation.
    pub config_validation: bool,
    /// Whether `PUT /configs` can safely apply a complete effective profile.
    pub full_config_reload: bool,
    /// Whether an individual runtime rule can be enabled or disabled.
    pub rule_toggle: bool,
    /// Whether the core implements the Mihomo native upgrade endpoint.
    pub core_upgrade: bool,
    /// Whether GeoData can be refreshed through the controller.
    pub geodata_update: bool,
    /// Whether the external controller UI can be refreshed through the controller.
    pub external_ui_update: bool,
    /// Whether the executable implements Mihomo ruleset conversion commands.
    pub ruleset_conversion: bool,
    /// Whether the connection API reports UDP flows completely.
    pub udp_connection_tracking: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mihomo_is_the_default_production_core() {
        assert_eq!(CoreKind::default(), CoreKind::Mihomo);
    }

    #[test]
    fn removed_and_unknown_runtime_names_are_rejected() {
        assert!("meow".parse::<CoreKind>().is_err());
        assert!("meow-rs".parse::<CoreKind>().is_err());
        assert!("auto".parse::<CoreKind>().is_err());
    }
}
