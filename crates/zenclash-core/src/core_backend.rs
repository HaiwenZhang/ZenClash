//! Runtime-core selection and explicit API capability declarations.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Runtime core launched or attached to by `ZenClash`.
///
/// Mihomo remains the production default. Meow is intentionally opt-in while
/// its controller API is still a compatible subset rather than a drop-in
/// implementation of every Mihomo extension.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoreKind {
    /// The Go Mihomo runtime with the complete controller feature set.
    #[default]
    Mihomo,
    /// The Rust meow-rs runtime, exposed as an experimental alternative.
    Meow,
}

impl CoreKind {
    /// Human-readable core name shown in native UI and diagnostics.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mihomo => "Mihomo",
            Self::Meow => "meow-rs",
        }
    }

    /// Executable filename without a platform-specific extension.
    #[must_use]
    pub const fn executable_stem(self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
            Self::Meow => "meow",
        }
    }

    /// Core-specific executable environment variable.
    #[must_use]
    pub const fn binary_environment_variable(self) -> &'static str {
        match self {
            Self::Mihomo => "ZENCLASH_MIHOMO_BINARY",
            Self::Meow => "ZENCLASH_MEOW_BINARY",
        }
    }

    /// Core-specific writable-home environment variable.
    #[must_use]
    pub const fn home_environment_variable(self) -> &'static str {
        match self {
            Self::Mihomo => "ZENCLASH_MIHOMO_HOME",
            Self::Meow => "ZENCLASH_MEOW_HOME",
        }
    }

    /// Controller features implemented reliably by this core.
    #[must_use]
    pub const fn capabilities(self) -> CoreCapabilities {
        match self {
            Self::Mihomo => CoreCapabilities {
                full_config_reload: true,
                rule_toggle: true,
                core_upgrade: true,
                geodata_update: true,
                external_ui_update: true,
                ruleset_conversion: true,
                udp_connection_tracking: true,
            },
            Self::Meow => CoreCapabilities {
                full_config_reload: false,
                rule_toggle: false,
                core_upgrade: false,
                geodata_update: false,
                external_ui_update: false,
                ruleset_conversion: false,
                udp_connection_tracking: false,
            },
        }
    }

    /// Whether the core is still presented as an experimental option.
    #[must_use]
    pub const fn is_experimental(self) -> bool {
        matches!(self, Self::Meow)
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
            "meow" | "meow-rs" | "meow_rs" => Ok(Self::Meow),
            _ => Err(ParseCoreKindError(value.trim().to_owned())),
        }
    }
}

/// Error returned when a runtime-core identifier is not recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseCoreKindError(String);

impl fmt::Display for ParseCoreKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "不支持的内核 `{}`；可选值为 mihomo 或 meow-rs",
            self.0
        )
    }
}

impl std::error::Error for ParseCoreKindError {}

/// Controller and command-line features that vary between runtime cores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreCapabilities {
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
        assert!(!CoreKind::default().is_experimental());
    }

    #[test]
    fn meow_aliases_parse_but_unknown_values_fail_loudly() {
        assert_eq!("meow".parse(), Ok(CoreKind::Meow));
        assert_eq!("meow-rs".parse(), Ok(CoreKind::Meow));
        assert!("auto".parse::<CoreKind>().is_err());
    }

    #[test]
    fn meow_does_not_claim_unimplemented_mihomo_extensions() {
        let capabilities = CoreKind::Meow.capabilities();

        assert!(!capabilities.full_config_reload);
        assert!(!capabilities.rule_toggle);
        assert!(!capabilities.core_upgrade);
        assert!(!capabilities.geodata_update);
        assert!(!capabilities.external_ui_update);
        assert!(!capabilities.ruleset_conversion);
        assert!(!capabilities.udp_connection_tracking);
    }
}
