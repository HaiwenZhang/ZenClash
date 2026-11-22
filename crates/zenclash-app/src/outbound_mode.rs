//! Shared outbound routing values and intent coordination used by application entry points.

use std::sync::Arc;

use zenclash_core::{CoreSession, CoreSessionError, LogMonitor, MihomoLogLevel};

mod coordinator;
pub use coordinator::OutboundModeCoordinator;

/// Mutually exclusive outbound routing modes accepted by the connected core.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutboundMode {
    /// Route connections through Mihomo rules.
    #[default]
    Rule,
    /// Route connections through the selected global proxy.
    Global,
    /// Bypass proxies for every connection.
    Direct,
}

impl OutboundMode {
    /// Returns the localized user-facing label.
    #[must_use]
    pub fn label(self) -> String {
        zenclash_i18n::text(match self {
            Self::Rule => "outbound_mode.rule",
            Self::Global => "outbound_mode.global",
            Self::Direct => "outbound_mode.direct",
        })
    }

    /// Returns the compact uppercase UI code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rule => "RULE",
            Self::Global => "GLOBAL",
            Self::Direct => "DIRECT",
        }
    }

    /// Returns the lowercase value accepted by Mihomo's `/configs` API.
    #[must_use]
    pub const fn api_value(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    /// Parses a Mihomo API value, defaulting unknown values to rule mode.
    #[must_use]
    pub fn from_api(mode: &str) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "global" => Self::Global,
            "direct" => Self::Direct,
            _ => Self::Rule,
        }
    }
}

/// Synchronizes outbound mode and log level from the current controller runtime,
/// discarding stale observations if the runtime or effective configuration changed.
///
/// # Errors
///
/// Returns a [`CoreSessionError`] when the controller observation cannot begin
/// or `/configs` cannot be read.
pub async fn synchronize_runtime_mode(
    session: CoreSession,
    mode: OutboundModeCoordinator,
    logs: Arc<LogMonitor>,
    intent_generation: u64,
) -> Result<(), CoreSessionError> {
    let observation = session.begin_observation().await?;
    let config = observation.client().runtime_config().await?;
    observation.identity().publish_if_current(|| {
        mode.synchronize(OutboundMode::from_api(&config.mode), intent_generation);
        if let Some(level) = MihomoLogLevel::from_api(&config.log_level) {
            logs.set_level(level);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests;
