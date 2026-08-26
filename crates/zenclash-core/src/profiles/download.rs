use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, USER_AGENT};

use super::{
    normalized_remote_url, ProfileStoreError, ProfileStoreResult, RemoteProfileOptions,
    RemoteProfileRoute, SubscriptionMetadata, SubscriptionUsage, MAX_PROFILE_BYTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
};

#[derive(Debug)]
pub(super) struct DownloadedProfile {
    pub(super) payload: String,
    pub(super) metadata: SubscriptionMetadata,
}

pub(super) async fn download_profile(
    url: &str,
    user_agent: &str,
    options: &RemoteProfileOptions,
    mihomo_proxy_port: Option<u16>,
) -> ProfileStoreResult<DownloadedProfile> {
    let parsed = reqwest::Url::parse(&normalized_remote_url(url)?)
        .map_err(|error| ProfileStoreError::InvalidYaml(format!("订阅 URL 无法规范化：{error}")))?;
    let timeout = validated_download_timeout(options.download_timeout_seconds)?;
    match options.route() {
        RemoteProfileRoute::Direct => {
            let client = subscription_client(timeout, None)?;
            download_with_client(&client, parsed, user_agent, options).await
        }
        RemoteProfileRoute::Mihomo => {
            let port = required_mihomo_proxy_port(mihomo_proxy_port)?;
            let client = subscription_client(timeout, Some(port))?;
            download_with_client(&client, parsed, user_agent, options).await
        }
        RemoteProfileRoute::DirectWithMihomoFallback => {
            let client = subscription_client(timeout, None)?;
            match download_with_client(&client, parsed.clone(), user_agent, options).await {
                Ok(downloaded) => Ok(downloaded),
                Err(direct_error) => {
                    let Some(port) = mihomo_proxy_port.filter(|port| *port != 0) else {
                        return Err(direct_error);
                    };
                    let Ok(client) = subscription_client(timeout, Some(port)) else {
                        return Err(direct_error);
                    };
                    download_with_client(&client, parsed, user_agent, options)
                        .await
                        .or(Err(direct_error))
                }
            }
        }
    }
}

fn validated_download_timeout(timeout_seconds: u32) -> ProfileStoreResult<Duration> {
    if !(MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS..=MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS)
        .contains(&timeout_seconds)
    {
        return Err(ProfileStoreError::InvalidYaml(format!(
            "订阅下载超时必须在 {MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS} 到 {MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS} 秒之间"
        )));
    }
    Ok(Duration::from_secs(u64::from(timeout_seconds)))
}

fn required_mihomo_proxy_port(port: Option<u16>) -> ProfileStoreResult<u16> {
    port.filter(|port| *port != 0).ok_or_else(|| {
        ProfileStoreError::InvalidYaml(
            "订阅要求通过 Mihomo 下载，但当前没有可用的 HTTP/Mixed 端口".into(),
        )
    })
}

fn subscription_client(
    timeout: Duration,
    proxy_port: Option<u16>,
) -> ProfileStoreResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(8));
    builder = if let Some(port) = proxy_port {
        builder.proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?)
    } else {
        builder.no_proxy()
    };
    Ok(builder.build()?)
}

async fn download_with_client(
    client: &reqwest::Client,
    url: reqwest::Url,
    user_agent: &str,
    options: &RemoteProfileOptions,
) -> ProfileStoreResult<DownloadedProfile> {
    let mut request = client
        .get(url)
        .header(USER_AGENT, user_agent)
        .header(ACCEPT, "text/yaml, application/yaml, text/plain, */*")
        .header(ACCEPT_ENCODING, "identity");
    if let Some(authorization) = &options.authorization {
        request = request.header(AUTHORIZATION, authorization.expose_secret());
    }
    let response = request.send().await?.error_for_status()?;
    let metadata = parse_subscription_metadata(response.headers());
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROFILE_BYTES as u64)
    {
        return Err(ProfileStoreError::InvalidYaml(format!(
            "订阅文件超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )));
    }
    let mut stream = response.bytes_stream();
    let mut payload = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if payload.len().saturating_add(chunk.len()) > MAX_PROFILE_BYTES {
            return Err(ProfileStoreError::InvalidYaml(format!(
                "订阅文件超过 {} MiB 限制",
                MAX_PROFILE_BYTES / 1024 / 1024
            )));
        }
        payload.extend_from_slice(&chunk);
    }
    let payload = String::from_utf8(payload)
        .map_err(|error| ProfileStoreError::InvalidYaml(format!("订阅内容不是 UTF-8：{error}")))?;
    Ok(DownloadedProfile { payload, metadata })
}

fn parse_subscription_metadata(headers: &reqwest::header::HeaderMap) -> SubscriptionMetadata {
    SubscriptionMetadata {
        usage: header_text(headers, "subscription-userinfo").and_then(parse_subscription_usage),
        home_url: header_text(headers, "profile-web-page-url").and_then(normalize_home_url),
        suggested_update_interval_minutes: header_text(headers, "profile-update-interval")
            .and_then(parse_update_interval),
    }
}

fn header_text<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn parse_subscription_usage(value: &str) -> Option<SubscriptionUsage> {
    let mut usage = SubscriptionUsage::default();
    let mut recognized = false;
    for part in value.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => usage.upload = value,
            "download" => usage.download = value,
            "total" => usage.total = value,
            "expire" => usage.expire = value,
            _ => continue,
        }
        recognized = true;
    }
    recognized.then_some(usage)
}

fn normalize_home_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
    {
        Some(url.into())
    } else {
        None
    }
}

fn parse_update_interval(value: &str) -> Option<u32> {
    let hours = value.parse::<f64>().ok()?;
    if !hours.is_finite() || hours <= 0.0 {
        return None;
    }
    let seconds = Duration::try_from_secs_f64(hours * 60.0 * 60.0)
        .ok()?
        .as_secs();
    let minutes = seconds.saturating_add(59) / 60;
    Some(
        u32::try_from(minutes)
            .unwrap_or(MAX_PROFILE_UPDATE_INTERVAL_MINUTES)
            .clamp(
                MIN_PROFILE_UPDATE_INTERVAL_MINUTES,
                MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subscription_headers_without_trusting_invalid_urls() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "subscription-userinfo",
            "upload=12; download=34; total=1000; expire=2000000000"
                .parse()
                .unwrap(),
        );
        headers.insert(
            "profile-web-page-url",
            "https://example.com/account".parse().unwrap(),
        );
        headers.insert("profile-update-interval", "6.5".parse().unwrap());

        let metadata = parse_subscription_metadata(&headers);

        let usage = metadata.usage.unwrap();
        assert_eq!(
            (usage.used(), usage.total, usage.expire),
            (46, 1000, 2_000_000_000)
        );
        assert_eq!(
            metadata.home_url.as_deref(),
            Some("https://example.com/account")
        );
        assert_eq!(metadata.suggested_update_interval_minutes, Some(390));
    }

    #[test]
    fn authorization_debug_output_is_redacted() {
        let authorization =
            super::super::SubscriptionAuthorization::new("Bearer private-token").unwrap();

        let debug = format!("{authorization:?}");

        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("private-token"));
    }
}
