use thiserror::Error;

mod archive;
mod service;
mod transaction;
mod workflow;

pub(crate) use workflow::InstalledCore;

#[cfg(test)]
mod tests;

pub use service::MihomoReleaseService;
pub use transaction::{CoreUpdateTransaction, PreparedCoreUpdate};

/// Result type used by versioned Mihomo core installation operations.
pub type CoreUpdateResult<T> = Result<T, CoreUpdateError>;

/// Failure while discovering, downloading, validating, or replacing a core.
#[derive(Debug, Error)]
pub enum CoreUpdateError {
    /// Application exit cancelled preparation or activation without restarting a child.
    #[error("{}", zenclash_i18n::text("core_page.errors.release_cancelled"))]
    Cancelled,
    /// GitHub or release-asset HTTP request failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_http", &[("error", .0.to_string())])
    )]
    Http(#[from] reqwest::Error),
    /// Release metadata is missing a trusted platform asset or digest.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_metadata", &[("error", .0.clone())])
    )]
    Metadata(String),
    /// A compressed or decompressed payload exceeded the safety limit.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_too_large", &[("error", .0.clone())])
    )]
    TooLarge(String),
    /// Downloaded bytes did not match GitHub's SHA-256 digest.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "core_page.errors.update_checksum",
            &[("expected", expected.clone()), ("actual", actual.clone())]
        )
    )]
    Checksum {
        /// Digest published in GitHub release metadata.
        expected: String,
        /// Digest calculated from the downloaded bytes.
        actual: String,
    },
    /// Gzip or ZIP extraction failed or did not contain the expected binary.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_archive", &[("error", .0.clone())])
    )]
    Archive(String),
    /// Candidate executable failed its `-v` identity/version check.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_candidate", &[("error", .0.clone())])
    )]
    Candidate(String),
    /// Filesystem staging, atomic replacement, or rollback failed.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_io", &[("error", .0.clone())])
    )]
    Io(String),
    /// Managed process could not be stopped, started, verified, or recovered.
    #[error(
        "{}",
        zenclash_i18n::text_with("core_page.errors.update_runtime", &[("error", .0.clone())])
    )]
    Runtime(String),
}

/// One installable GitHub release for the current operating system and CPU.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MihomoRelease {
    /// Git tag such as `v1.19.30`.
    pub tag: String,
    /// ISO timestamp reported by GitHub.
    pub published_at: String,
    /// Whether GitHub marks the release as a prerelease.
    pub prerelease: bool,
    /// Trusted platform-specific archive selected from the release assets.
    pub asset: MihomoReleaseAsset,
}

/// Platform archive and integrity metadata selected from a GitHub release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MihomoReleaseAsset {
    /// GitHub asset file name.
    pub name: String,
    /// Official browser download URL.
    pub download_url: reqwest::Url,
    /// Compressed byte size declared by GitHub.
    pub size: u64,
    /// Lowercase SHA-256 digest without the `sha256:` prefix.
    pub sha256: String,
}
