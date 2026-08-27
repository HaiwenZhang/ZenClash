//! Read-only ZenClash application update discovery.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/HaiwenZhang/zenclash/releases/latest";
const MAX_RELEASE_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_NOTES_CHARS: usize = 8_192;

/// Failure while discovering or validating the latest ZenClash release.
#[derive(Debug, Error)]
pub enum AppUpdateError {
    /// GitHub or the configured test endpoint could not be reached.
    #[error("应用更新网络请求失败：{0}")]
    Http(#[from] reqwest::Error),
    /// Release metadata, version, or link did not satisfy the trusted policy.
    #[error("应用 Release 元数据无效：{0}")]
    Metadata(String),
    /// The bounded release response exceeded its accepted size.
    #[error("应用 Release 元数据超过大小限制")]
    TooLarge,
}

/// Result type for application update discovery.
pub type AppUpdateResult<T> = Result<T, AppUpdateError>;

/// Validated latest ZenClash release presented for user-confirmed download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppRelease {
    /// Git tag returned by the official release.
    pub tag: String,
    /// Display name from the release metadata.
    pub name: String,
    /// Bounded release notes shown in the application.
    pub notes: String,
    /// ISO timestamp returned by GitHub.
    pub published_at: String,
    /// Official HTTPS release page; ZenClash never opens an asset URL directly.
    pub page_url: String,
}

/// Comparison between the running application and the latest stable release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppUpdateStatus {
    /// The official repository does not currently expose a stable release.
    NoPublishedRelease {
        /// Current application version.
        current: String,
    },
    /// The running version is not older than the latest stable release.
    UpToDate {
        /// Current application version.
        current: String,
        /// Latest stable release tag.
        latest: String,
    },
    /// A newer stable release is available for user-confirmed download.
    Available {
        /// Current application version.
        current: String,
        /// Validated release metadata and official page.
        release: AppRelease,
    },
}

/// GitHub-backed, notification-only application update service.
#[derive(Clone)]
pub struct AppUpdateService {
    http: reqwest::Client,
    latest_url: reqwest::Url,
    allow_insecure_metadata: bool,
}

impl AppUpdateService {
    /// Creates the official ZenClash release service.
    ///
    /// # Errors
    ///
    /// Returns an error if the fixed endpoint or bounded HTTP client cannot be built.
    pub fn new() -> AppUpdateResult<Self> {
        Self::with_latest_url(LATEST_RELEASE_URL, false)
    }

    fn with_latest_url(url: &str, allow_insecure_metadata: bool) -> AppUpdateResult<Self> {
        let latest_url = reqwest::Url::parse(url)
            .map_err(|error| AppUpdateError::Metadata(format!("API URL 无效：{error}")))?;
        if !allow_insecure_metadata {
            validate_official_release_api_url(&latest_url)?;
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(4))
            .build()?;
        Ok(Self {
            http,
            latest_url,
            allow_insecure_metadata,
        })
    }

    /// Checks the latest stable release without downloading an executable.
    ///
    /// # Errors
    ///
    /// Returns an error for HTTP, size, version, or official-link validation failures.
    pub async fn check(&self, current: &str) -> AppUpdateResult<AppUpdateStatus> {
        let response = self
            .http
            .get(self.latest_url.clone())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        if !self.allow_insecure_metadata {
            validate_official_release_api_url(response.url())?;
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(AppUpdateStatus::NoPublishedRelease {
                current: current.to_owned(),
            });
        }
        let response = response.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RELEASE_METADATA_BYTES)
        {
            return Err(AppUpdateError::TooLarge);
        }
        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_RELEASE_METADATA_BYTES {
            return Err(AppUpdateError::TooLarge);
        }
        let release = serde_json::from_slice::<RawAppRelease>(&bytes)
            .map_err(|error| AppUpdateError::Metadata(format!("Release JSON 无法解析：{error}")))?;
        compare_release(current, release)
    }
}

#[derive(Deserialize)]
struct RawAppRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn compare_release(current: &str, release: RawAppRelease) -> AppUpdateResult<AppUpdateStatus> {
    if release.draft || release.prerelease {
        return Err(AppUpdateError::Metadata(
            "latest endpoint 返回了 draft 或 prerelease".into(),
        ));
    }
    let current_key = VersionKey::parse(current)?;
    let latest_key = VersionKey::parse(&release.tag_name)?;
    let page_url = validate_official_release_url(&release.html_url)?;
    if latest_key <= current_key {
        return Ok(AppUpdateStatus::UpToDate {
            current: current.to_owned(),
            latest: release.tag_name,
        });
    }
    let notes = truncate_chars(
        release.body.as_deref().unwrap_or_default(),
        MAX_RELEASE_NOTES_CHARS,
    );
    Ok(AppUpdateStatus::Available {
        current: current.to_owned(),
        release: AppRelease {
            name: release
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| release.tag_name.clone()),
            tag: release.tag_name,
            notes,
            published_at: release.published_at.unwrap_or_default(),
            page_url,
        },
    })
}

/// Validates an external link before handing it to the operating system.
///
/// Only credential-free HTTPS links are accepted. Callers handling a more
/// privileged destination should additionally enforce its host and path.
///
/// # Errors
///
/// Returns a metadata error when the value is not a credential-free HTTPS URL.
pub fn validate_external_https_url(value: &str) -> AppUpdateResult<String> {
    let url = reqwest::Url::parse(value)
        .map_err(|error| AppUpdateError::Metadata(format!("外链 URL 无效：{error}")))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(AppUpdateError::Metadata(
            "外链只允许无凭据的 HTTPS URL".into(),
        ));
    }
    Ok(url.to_string())
}

fn validate_official_release_url(value: &str) -> AppUpdateResult<String> {
    let normalized = validate_external_https_url(value)?;
    let url = reqwest::Url::parse(&normalized)
        .map_err(|error| AppUpdateError::Metadata(format!("Release URL 无效：{error}")))?;
    if !url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
        || !url
            .path()
            .starts_with("/HaiwenZhang/zenclash/releases/tag/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppUpdateError::Metadata(
            "Release 页面不是 ZenClash 官方 GitHub tag 页面".into(),
        ));
    }
    Ok(normalized)
}

fn validate_official_release_api_url(url: &reqwest::Url) -> AppUpdateResult<()> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("api.github.com"))
        || url.path() != "/repos/HaiwenZhang/zenclash/releases/latest"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppUpdateError::Metadata(
            "Release API 不是 ZenClash 官方 GitHub latest endpoint".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct VersionKey {
    numbers: [u64; 4],
    stable: bool,
    prerelease: String,
}

impl VersionKey {
    fn parse(value: &str) -> AppUpdateResult<Self> {
        let value = value.trim().trim_start_matches('v');
        let value = value.split_once('+').map_or(value, |(version, _)| version);
        let (numbers, prerelease) = value
            .split_once('-')
            .map_or((value, ""), |(numbers, prerelease)| (numbers, prerelease));
        let parsed = numbers
            .split('.')
            .map(|part| {
                part.parse::<u64>()
                    .map_err(|_| AppUpdateError::Metadata(format!("版本号格式错误：{value}")))
            })
            .collect::<AppUpdateResult<Vec<_>>>()?;
        if parsed.is_empty() || parsed.len() > 4 {
            return Err(AppUpdateError::Metadata(format!("版本号格式错误：{value}")));
        }
        let mut numbers = [0_u64; 4];
        numbers[..parsed.len()].copy_from_slice(&parsed);
        Ok(Self {
            numbers,
            stable: prerelease.is_empty(),
            prerelease: prerelease.to_owned(),
        })
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[test]
    fn version_comparison_prefers_stable_and_numeric_components() {
        assert!(VersionKey::parse("v1.10.0").unwrap() > VersionKey::parse("1.9.9").unwrap());
        assert!(VersionKey::parse("1.0.0").unwrap() > VersionKey::parse("1.0.0-beta.1").unwrap());
        assert!(VersionKey::parse("release").is_err());
    }

    #[test]
    fn external_links_require_https_without_credentials() {
        assert!(validate_external_https_url("https://github.com/project/releases").is_ok());
        for rejected in [
            "http://github.com/project/releases",
            "file:///tmp/release",
            "javascript:alert(1)",
            "https://user:secret@github.com/project/releases",
        ] {
            assert!(validate_external_https_url(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn release_metadata_rejects_untrusted_api_hosts_and_redirect_targets() {
        assert!(
            validate_official_release_api_url(
                &reqwest::Url::parse(
                    "https://api.github.com/repos/HaiwenZhang/zenclash/releases/latest"
                )
                .unwrap()
            )
            .is_ok()
        );
        for value in [
            "https://github.example/repos/HaiwenZhang/zenclash/releases/latest",
            "http://api.github.com/repos/HaiwenZhang/zenclash/releases/latest",
            "https://api.github.com/repos/other/zenclash/releases/latest",
            "https://api.github.com/repos/HaiwenZhang/zenclash/releases/latest?token=secret",
        ] {
            assert!(
                validate_official_release_api_url(&reqwest::Url::parse(value).unwrap()).is_err()
            );
        }
    }

    #[tokio::test]
    async fn latest_release_is_notification_only_and_uses_the_official_page() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let bytes = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..bytes]).starts_with("GET /latest "));
            let body = serde_json::json!({
                "tag_name": "v0.2.0",
                "name": "ZenClash 0.2.0",
                "body": "Release notes",
                "html_url": "https://github.com/HaiwenZhang/zenclash/releases/tag/v0.2.0",
                "published_at": "2026-08-27T00:00:00Z",
                "draft": false,
                "prerelease": false
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let service =
            AppUpdateService::with_latest_url(&format!("http://{address}/latest"), true).unwrap();

        let status = service.check("0.1.0").await.unwrap();
        server.join().unwrap();

        let AppUpdateStatus::Available { release, .. } = status else {
            panic!("expected an update notification")
        };
        assert_eq!(release.notes, "Release notes");
        assert_eq!(
            release.page_url,
            "https://github.com/HaiwenZhang/zenclash/releases/tag/v0.2.0"
        );
    }

    #[tokio::test]
    async fn missing_official_release_is_a_stable_empty_state() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let service =
            AppUpdateService::with_latest_url(&format!("http://{address}/"), true).unwrap();

        assert_eq!(
            service.check("0.1.0").await.unwrap(),
            AppUpdateStatus::NoPublishedRelease {
                current: "0.1.0".into()
            }
        );
        server.join().unwrap();
    }

    #[test]
    fn untrusted_release_page_is_rejected() {
        let error = compare_release(
            "0.1.0",
            RawAppRelease {
                tag_name: "v0.2.0".into(),
                name: None,
                body: None,
                html_url: "https://example.com/download?token=raw".into(),
                published_at: None,
                draft: false,
                prerelease: false,
            },
        )
        .unwrap_err();

        assert!(matches!(error, AppUpdateError::Metadata(_)));
    }
}
