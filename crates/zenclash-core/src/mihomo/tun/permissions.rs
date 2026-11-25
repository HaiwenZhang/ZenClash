use std::path::PathBuf;

use thiserror::Error;

/// Result type used by native TUN permission operations.
pub type TunPermissionResult<T> = Result<T, TunPermissionError>;

/// Failure while observing Mihomo TUN privilege authority.
#[derive(Debug, Error)]
pub enum TunPermissionError {
    /// The configured path is absent, invalid, or is not a supported Mihomo core.
    #[error(
        "{}",
        zenclash_i18n::text_with("tun.errors.invalid_binary", &[("error", .0.clone())])
    )]
    InvalidBinary(String),
    /// The operating system does not provide a supported privilege workflow.
    #[error(
        "{}",
        zenclash_i18n::text_with("tun.errors.unsupported", &[("error", .0.clone())])
    )]
    Unsupported(String),
    /// A native permission command or privilege prompt failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("tun.errors.platform", &[("error", .0.clone())])
    )]
    Platform(String),
    /// The native operation returned successfully but the permission readback failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("tun.errors.verification", &[("error", .0.clone())])
    )]
    Verification(String),
}

/// Point-in-time privilege state for the selected Mihomo executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunPermissionStatus {
    /// Whether the current core can create a TUN device without another prompt.
    pub granted: bool,
    /// Whether `ZenClash` can offer a native, user-confirmed authorization action.
    pub can_request: bool,
    /// Canonical Mihomo executable belonging to the observed local runtime.
    pub binary: PathBuf,
    /// Human-readable platform evidence behind the state.
    pub detail: String,
}
