use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, USER_AGENT};

use super::{ProfileStoreError, ProfileStoreResult, MAX_PROFILE_BYTES};

pub(super) async fn download_profile(url: &str, user_agent: &str) -> ProfileStoreResult<String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| ProfileStoreError::InvalidYaml(format!("订阅 URL 无效：{error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ProfileStoreError::InvalidYaml(
            "订阅 URL 仅支持 HTTP 或 HTTPS".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client
        .get(parsed)
        .header(USER_AGENT, user_agent)
        .header(ACCEPT, "text/yaml, application/yaml, text/plain, */*")
        .send()
        .await?
        .error_for_status()?;
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
    String::from_utf8(payload)
        .map_err(|error| ProfileStoreError::InvalidYaml(format!("订阅内容不是 UTF-8：{error}")))
}
