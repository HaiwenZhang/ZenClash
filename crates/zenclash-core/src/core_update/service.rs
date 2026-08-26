use std::{path::Path, time::Duration};

use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    archive::prepare_downloaded, transaction::PreparedCoreUpdate, CoreUpdateError,
    CoreUpdateResult, MihomoRelease, MihomoReleaseAsset,
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
        let api_base = reqwest::Url::parse(api_base)
            .map_err(|error| CoreUpdateError::Metadata(format!("API URL 无效：{error}")))?;
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
            return Err(CoreUpdateError::Metadata(format!(
                "版本数量必须在 1 到 {MAX_RELEASES} 之间"
            )));
        }
        let url = self
            .api_base
            .join("releases")
            .map_err(|error| CoreUpdateError::Metadata(format!("Release URL 无效：{error}")))?;
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
            return Err(CoreUpdateError::TooLarge(format!(
                "Release 元数据超过 {} MiB",
                MAX_METADATA_BYTES / 1024 / 1024
            )));
        }
        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_METADATA_BYTES {
            return Err(CoreUpdateError::TooLarge(format!(
                "Release 元数据实际响应超过 {} MiB",
                MAX_METADATA_BYTES / 1024 / 1024
            )));
        }
        let raw = serde_json::from_slice::<Vec<RawRelease>>(&bytes).map_err(|error| {
            CoreUpdateError::Metadata(format!("Release JSON 无法解析：{error}"))
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
        validate_asset_url(&release.asset.download_url, self.allow_insecure_assets)?;
        let target = std::fs::canonicalize(target.as_ref()).map_err(|error| {
            CoreUpdateError::Io(format!(
                "无法解析当前内核 {}：{error}",
                target.as_ref().display()
            ))
        })?;
        if !target.is_file() {
            return Err(CoreUpdateError::Io(format!(
                "当前内核不是普通文件：{}",
                target.display()
            )));
        }
        let archive = self.download(&release.asset).await?;
        let release = release.clone();
        tokio::task::spawn_blocking(move || prepare_downloaded(&release, &target, &archive))
            .await
            .map_err(|error| CoreUpdateError::Io(format!("候选内核任务异常结束：{error}")))?
    }

    async fn download(&self, asset: &MihomoReleaseAsset) -> CoreUpdateResult<Vec<u8>> {
        let declared = usize::try_from(asset.size)
            .map_err(|_| CoreUpdateError::TooLarge(format!("{} 无法表示", asset.size)))?;
        if declared > MAX_ARCHIVE_BYTES {
            return Err(CoreUpdateError::TooLarge(format!(
                "{} 为 {} MiB，上限为 {} MiB",
                asset.name,
                declared / 1024 / 1024,
                MAX_ARCHIVE_BYTES / 1024 / 1024
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
            return Err(CoreUpdateError::TooLarge(format!(
                "{} 的 HTTP Content-Length 超过 {} MiB",
                asset.name,
                MAX_ARCHIVE_BYTES / 1024 / 1024
            )));
        }
        let mut bytes = Vec::with_capacity(declared.min(MAX_ARCHIVE_BYTES));
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > MAX_ARCHIVE_BYTES {
                return Err(CoreUpdateError::TooLarge(format!(
                    "{} 的实际响应超过 {} MiB",
                    asset.name,
                    MAX_ARCHIVE_BYTES / 1024 / 1024
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
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
            CoreUpdateError::Metadata(format!("{} 下载 URL 无效：{error}", asset.name))
        })?;
        validate_asset_url(&download_url, self.allow_insecure_assets)?;
        let Some(digest) = asset.digest else {
            return Ok(None);
        };
        let sha256 = parse_digest(&digest)?;
        if asset.size > MAX_ARCHIVE_BYTES as u64 {
            return Err(CoreUpdateError::TooLarge(format!(
                "{} 声明大小超过 {} MiB",
                asset.name,
                MAX_ARCHIVE_BYTES / 1024 / 1024
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
        return Err(CoreUpdateError::Metadata(format!(
            "版本标签不安全：{tag:?}"
        )));
    }
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "mihomo-darwin-arm64",
        ("macos", "x86_64") => "mihomo-darwin-amd64-compatible",
        ("linux", "aarch64") => "mihomo-linux-arm64",
        ("linux", "x86_64") => "mihomo-linux-amd64-compatible",
        ("windows", "aarch64") => "mihomo-windows-arm64",
        ("windows", "x86_64") => "mihomo-windows-amd64-compatible",
        ("windows", "x86") => "mihomo-windows-386",
        (os, arch) => {
            return Err(CoreUpdateError::Metadata(format!(
                "不支持的平台：{os}-{arch}"
            )))
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
        return Err(CoreUpdateError::Metadata(format!(
            "拒绝不可信的 GitHub 资产 URL：{url}"
        )));
    }
    Ok(())
}

pub(super) fn parse_digest(digest: &str) -> CoreUpdateResult<String> {
    let Some(value) = digest.strip_prefix("sha256:") else {
        return Err(CoreUpdateError::Metadata(
            "Release 资产缺少 sha256 digest".into(),
        ));
    };
    let value = value.to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreUpdateError::Metadata(format!(
            "SHA-256 digest 格式错误：{digest}"
        )));
    }
    Ok(value)
}

pub(super) fn verify_sha256(bytes: &[u8], expected: &str) -> CoreUpdateResult<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(CoreUpdateError::Checksum {
            expected: expected.to_owned(),
            actual,
        })
    }
}
