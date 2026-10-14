//! Desired native state, separate from persisted settings and verified ownership.
use std::borrow::Cow;

use super::{SystemProxyMode, SystemProxySettings};

/// A proposed native system-proxy change; it carries no ownership or write authority.
///
/// Enabled configurations borrow settings at typed entry points. Compatibility
/// entry points own their settings once, without forwarding positional fields.
#[derive(Debug)]
pub struct SystemProxyConfiguration<'a> {
    pub(super) kind: SystemProxyConfigurationKind<'a>,
}

#[derive(Debug)]
pub(super) enum SystemProxyConfigurationKind<'a> {
    Disabled,
    Enabled {
        settings: Cow<'a, SystemProxySettings>,
        port: u16,
    },
}

impl<'a> SystemProxyConfiguration<'a> {
    /// Proposes disabling the native proxy without irrelevant endpoint settings.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            kind: SystemProxyConfigurationKind::Disabled,
        }
    }

    /// Proposes enabling the selected settings at the current core proxy port.
    ///
    /// Validation happens within the existing native transaction, preserving
    /// service detection, recovery, and error precedence.
    #[must_use]
    pub fn enabled(settings: &'a SystemProxySettings, port: u16) -> Self {
        Self {
            kind: SystemProxyConfigurationKind::Enabled {
                settings: Cow::Borrowed(settings),
                port,
            },
        }
    }

    /// Returns whether this configuration proposes an enabled native proxy.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self.kind, SystemProxyConfigurationKind::Enabled { .. })
    }

    pub(super) fn from_legacy(
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> Self {
        if !enabled {
            return Self::disabled();
        }
        Self {
            kind: SystemProxyConfigurationKind::Enabled {
                settings: Cow::Owned(SystemProxySettings {
                    mode,
                    host: host.into(),
                    bypass: bypass.into(),
                    pac_script: pac_script.into(),
                }),
                port,
            },
        }
    }
}
