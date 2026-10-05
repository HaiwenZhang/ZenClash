// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; original authors and changes are recorded in NOTICE.md.
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
pub enum RunningMode {
    Service,
    Sidecar,
    NotRunning,
}

use std::fmt;

impl fmt::Display for RunningMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service => write!(f, "Service"),
            Self::Sidecar => write!(f, "Sidecar"),
            Self::NotRunning => write!(f, "NotRunning"),
        }
    }
}
