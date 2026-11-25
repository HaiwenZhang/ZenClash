// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; original authors and changes are recorded in NOTICE.md.
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
/// The backend currently running the application-owned core.
pub enum RunningMode {
    /// The authenticated privileged service owns the core.
    Service,
    /// The desktop application owns a local child process.
    Sidecar,
    /// No core is currently owned by this application session.
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
