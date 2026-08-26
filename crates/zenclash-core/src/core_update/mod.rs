use thiserror::Error;

mod archive;
mod service;
mod transaction;
mod workflow;

#[cfg(test)]
mod tests;

pub use service::MihomoReleaseService;
pub use transaction::{CoreUpdateTransaction, PreparedCoreUpdate};

/// Result type used by versioned Mihomo core installation operations.
pub type CoreUpdateResult<T> = Result<T, CoreUpdateError>;

/// Failure while discovering, downloading, validating, or replacing a core.
#[derive(Debug, Error)]
pub enum CoreUpdateError {
    /// GitHub or release-asset HTTP request failed.
    #[error("内核更新网络请求失败：{0}")]
    Http(#[from] reqwest::Error),
    /// Release metadata is missing a trusted platform asset or digest.
    #[error("内核 Release 元数据无效：{0}")]
    Metadata(String),
    /// A compressed or decompressed payload exceeded the safety limit.
    #[error("内核更新文件超过大小限制：{0}")]
    TooLarge(String),
    /// Downloaded bytes did not match GitHub's SHA-256 digest.
    #[error("内核更新 SHA-256 校验失败：期望 {expected}，实际 {actual}")]
    Checksum {
        /// Digest published in GitHub release metadata.
        expected: String,
        /// Digest calculated from the downloaded bytes.
        actual: String,
    },
    /// Gzip or ZIP extraction failed or did not contain the expected binary.
    #[error("内核更新压缩包无效：{0}")]
    Archive(String),
    /// Candidate executable failed its `-v` identity/version check.
    #[error("候选 Mihomo 内核验证失败：{0}")]
    Candidate(String),
    /// Filesystem staging, atomic replacement, or rollback failed.
    #[error("内核更新文件事务失败：{0}")]
    Io(String),
    /// Managed process could not be stopped, started, verified, or recovered.
    #[error("内核更新运行时切换失败：{0}")]
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
