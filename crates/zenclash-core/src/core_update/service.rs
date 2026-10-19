use std::{path::Path, time::Duration};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    CoreUpdateError, CoreUpdateResult, MihomoRelease, MihomoReleaseAsset,
    archive::prepare_downloaded, transaction::PreparedCoreUpdate,
};

const DEFAULT_API_BASE: &str = "https://api.github.com/repos/MetaCubeX/mihomo/";
const MAX_RELEASES: usize = 50;
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;

/// GitHub-backed release catalog and bounded archive downloader.
#[derive(Clone)]
pub struct MihomoReleaseService {
    http: reqwest::Client,
    api_base: reqwest::Url,
    allow_insecure_assets: bool,
}

impl MihomoReleaseService {
    /// Creates the official `MetaCubeX` GitHub release service.
    ///
    /// # Errors
    ///
    /// Returns an error when the fixed API URL or HTTP client cannot be built.
    pub fn new() -> CoreUpdateResult<Self> {
        Self::with_base(DEFAULT_API_BASE, false)
    }

    pub(super) fn with_base(api_base: &str, allow_insecure_assets: bool) -> CoreUpdateResult<Self> {
        let api_base = reqwest::Url::parse(api_base).map_err(|error| {
            CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.api_url",
                &[("error", error.to_string())],
            ))
        })?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()?;
        Ok(Self {
            http,
            api_base,
            allow_insecure_assets,
        })
    }

    /// Fetches installable releases for the current operating system and CPU.
    ///
    /// Draft releases and releases without a matching trusted archive are
    /// excluded. Matching assets must include GitHub's SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid limit, HTTP/API failure, unsafe download
    /// URL, malformed digest, unsupported platform, or oversized asset.
    pub async fn releases(&self, limit: usize) -> CoreUpdateResult<Vec<MihomoRelease>> {
        if !(1..=MAX_RELEASES).contains(&limit) {
            return Err(CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.release_count",
                &[("maximum", MAX_RELEASES.to_string())],
            )));
        }
        let url = self.api_base.join("releases").map_err(|error| {
            CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.release_url",
                &[("error", error.to_string())],
            ))
        })?;
        let response = self
            .http
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .query(&[("per_page", limit)])
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_METADATA_BYTES)
        {
            return Err(CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                "core_update.details.metadata_limit",
                &[("limit", (MAX_METADATA_BYTES / 1024 / 1024).to_string())],
            )));
        }
        let bytes = crate::http_body::read_limited(response, MAX_METADATA_BYTES as usize)
            .await
            .map_err(|error| match error {
                crate::http_body::ReadBodyError::Http(error) => CoreUpdateError::Http(error),
                crate::http_body::ReadBodyError::TooLarge => {
                    CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                        "core_update.details.metadata_response_limit",
                        &[("limit", (MAX_METADATA_BYTES / 1024 / 1024).to_string())],
                    ))
                }
            })?;
        let raw = serde_json::from_slice::<Vec<RawRelease>>(&bytes).map_err(|error| {
            CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.release_json",
                &[("error", error.to_string())],
            ))
        })?;
        let mut releases = Vec::new();
        for release in raw.into_iter().filter(|release| !release.draft) {
            if let Some(release) = self.select_release(release)? {
                releases.push(release);
            }
        }
        Ok(releases)
    }

    /// Downloads, hashes, extracts, and validates a release into a same-folder
    /// staging file without changing the active executable.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe metadata, download/size/checksum failures,
    /// archive errors, filesystem failures, or a candidate whose `-v` output
    /// does not identify the requested Mihomo version.
    pub async fn prepare(
        &self,
        release: &MihomoRelease,
        target: impl AsRef<Path>,
    ) -> CoreUpdateResult<PreparedCoreUpdate> {
        self.prepare_until(release, target.as_ref().to_path_buf(), None)
            .await
    }

    pub(crate) async fn prepare_until(
        &self,
        release: &MihomoRelease,
        target: std::path::PathBuf,
        cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> CoreUpdateResult<PreparedCoreUpdate> {
        validate_asset_url(&release.asset.download_url, self.allow_insecure_assets)?;
        let archive = tokio::select! {
            biased;
            () = super::workflow::wait_for_cancellation(cancelled.clone()) => {
                return Err(CoreUpdateError::Cancelled);
            }
            result = self.download(&release.asset) => result?,
        };
        let release = release.clone();
        tokio::task::spawn_blocking(move || {
            if super::workflow::is_cancelled(&cancelled) {
                return Err(CoreUpdateError::Cancelled);
            }
            let target = std::fs::canonicalize(&target).map_err(|error| {
                CoreUpdateError::Io(zenclash_i18n::text_with(
                    "core_update.details.resolve_target",
                    &[
                        ("path", target.display().to_string()),
                        ("error", error.to_string()),
                    ],
                ))
            })?;
            if !target.is_file() {
                return Err(CoreUpdateError::Io(zenclash_i18n::text_with(
                    "core_update.details.invalid_target",
                    &[("path", target.display().to_string())],
                )));
            }
            let prepared = prepare_downloaded(&release, &target, &archive)?;
            if super::workflow::is_cancelled(&cancelled) {
                return Err(CoreUpdateError::Cancelled);
            }
            Ok(prepared)
        })
        .await
        .map_err(|error| {
            CoreUpdateError::Io(zenclash_i18n::text_with(
                "core_update.details.candidate_task",
                &[("error", error.to_string())],
            ))
        })?
    }

    async fn download(&self, asset: &MihomoReleaseAsset) -> CoreUpdateResult<Vec<u8>> {
        let declared = usize::try_from(asset.size).map_err(|_| {
            CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                "core_update.details.unrepresentable_size",
                &[("size", asset.size.to_string())],
            ))
        })?;
        if declared > MAX_ARCHIVE_BYTES {
            return Err(CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                "core_update.details.archive_limit",
                &[
                    ("name", asset.name.clone()),
                    ("size", (declared / 1024 / 1024).to_string()),
                    ("limit", (MAX_ARCHIVE_BYTES / 1024 / 1024).to_string()),
                ],
            )));
        }
        let response = self
            .http
            .get(asset.download_url.clone())
            .send()
            .await?
            .error_for_status()?;
        validate_asset_url(response.url(), self.allow_insecure_assets)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ARCHIVE_BYTES as u64)
        {
            return Err(CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                "core_update.details.content_length_limit",
                &[
                    ("name", asset.name.clone()),
                    ("limit", (MAX_ARCHIVE_BYTES / 1024 / 1024).to_string()),
                ],
            )));
        }
        let bytes = crate::http_body::read_limited(response, MAX_ARCHIVE_BYTES)
            .await
            .map_err(|error| match error {
                crate::http_body::ReadBodyError::Http(error) => CoreUpdateError::Http(error),
                crate::http_body::ReadBodyError::TooLarge => {
                    CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                        "core_update.details.download_response_limit",
                        &[
                            ("name", asset.name.clone()),
                            ("limit", (MAX_ARCHIVE_BYTES / 1024 / 1024).to_string()),
                        ],
                    ))
                }
            })?;
        verify_sha256(&bytes, &asset.sha256)?;
        Ok(bytes)
    }

    pub(super) fn select_release(
        &self,
        release: RawRelease,
    ) -> CoreUpdateResult<Option<MihomoRelease>> {
        let expected_name = platform_asset_name(&release.tag_name)?;
        let Some(asset) = release
            .assets
            .into_iter()
            .find(|asset| asset.name == expected_name)
        else {
            return Ok(None);
        };
        let download_url = reqwest::Url::parse(&asset.browser_download_url).map_err(|error| {
            CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.asset_url",
                &[("name", asset.name.clone()), ("error", error.to_string())],
            ))
        })?;
        validate_asset_url(&download_url, self.allow_insecure_assets)?;
        let Some(digest) = asset.digest else {
            return Ok(None);
        };
        let sha256 = parse_digest(&digest)?;
        if asset.size > MAX_ARCHIVE_BYTES as u64 {
            return Err(CoreUpdateError::TooLarge(zenclash_i18n::text_with(
                "core_update.details.declared_asset_limit",
                &[
                    ("name", asset.name.clone()),
                    ("limit", (MAX_ARCHIVE_BYTES / 1024 / 1024).to_string()),
                ],
            )));
        }
        Ok(Some(MihomoRelease {
            tag: release.tag_name,
            published_at: release.published_at.unwrap_or_default(),
            prerelease: release.prerelease,
            asset: MihomoReleaseAsset {
                name: asset.name,
                download_url,
                size: asset.size,
                sha256,
            },
        }))
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct RawRelease {
    pub(super) tag_name: String,
    #[serde(default)]
    pub(super) published_at: Option<String>,
    #[serde(default)]
    pub(super) prerelease: bool,
    #[serde(default)]
    pub(super) draft: bool,
    #[serde(default)]
    pub(super) assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    #[serde(default)]
    pub(super) size: u64,
    #[serde(default)]
    pub(super) digest: Option<String>,
}

pub(super) fn platform_asset_name(tag: &str) -> CoreUpdateResult<String> {
    if tag.trim().is_empty() || tag.contains(['/', '\\']) {
        return Err(CoreUpdateError::Metadata(zenclash_i18n::text_with(
            "core_update.details.unsafe_tag",
            &[("tag", format!("{tag:?}"))],
        )));
    }
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "mihomo-darwin-arm64",
        ("macos", "x86_64") => "mihomo-darwin-amd64-v1",
        ("linux", "aarch64") => "mihomo-linux-arm64",
        ("linux", "x86_64") => "mihomo-linux-amd64-v1",
        ("windows", "aarch64") => "mihomo-windows-arm64",
        ("windows", "x86_64") => "mihomo-windows-amd64-v1",
        ("windows", "x86") => "mihomo-windows-386",
        (os, arch) => {
            return Err(CoreUpdateError::Metadata(zenclash_i18n::text_with(
                "core_update.details.unsupported_platform",
                &[("os", os.to_owned()), ("arch", arch.to_owned())],
            )));
        }
    };
    let extension = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "gz"
    };
    Ok(format!("{platform}-{tag}.{extension}"))
}

pub(super) fn validate_asset_url(url: &reqwest::Url, allow_insecure: bool) -> CoreUpdateResult<()> {
    if allow_insecure {
        return Ok(());
    }
    let trusted_host = matches!(
        url.host_str(),
        Some(
            "github.com"
                | "objects.githubusercontent.com"
                | "release-assets.githubusercontent.com"
                | "github-releases.githubusercontent.com"
        )
    );
    if url.scheme() != "https"
        || !trusted_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CoreUpdateError::Metadata(zenclash_i18n::text_with(
            "core_update.details.untrusted_url",
            &[("url", url.to_string())],
        )));
    }
    Ok(())
}

pub(super) fn parse_digest(digest: &str) -> CoreUpdateResult<String> {
    let Some(value) = digest.strip_prefix("sha256:") else {
        return Err(CoreUpdateError::Metadata(zenclash_i18n::text(
            "core_update.details.missing_digest",
        )));
    };
    let value = value.to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreUpdateError::Metadata(zenclash_i18n::text_with(
            "core_update.details.invalid_digest",
            &[("digest", digest.to_owned())],
        )));
    }
    Ok(value)
}

pub(super) fn verify_sha256(bytes: &[u8], expected: &str) -> CoreUpdateResult<()> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual == expected {
        Ok(())
    } else {
        Err(CoreUpdateError::Checksum {
            expected: expected.to_owned(),
            actual,
        })
    }
}
