use reqwest::{Method, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    proxy::RawProxyCatalog, ConnectionsSnapshot, DelayResult, MihomoEndpoint, ProviderCatalog,
    ProxyCatalog, RuleCatalog, RuntimeConfig,
};

pub type MihomoResult<T> = Result<T, MihomoError>;

#[derive(Debug, Error)]
pub enum MihomoError {
    #[error("invalid Mihomo controller endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("Mihomo request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Mihomo process error: {0}")]
    Process(String),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct VersionInfo {
    #[serde(default)]
    pub meta: bool,
    #[serde(default)]
    pub version: String,
}

/// Typed HTTP access to Mihomo's external-controller API.
#[derive(Clone)]
pub struct MihomoClient {
    endpoint: MihomoEndpoint,
    http: reqwest::Client,
}

impl MihomoClient {
    pub fn new(endpoint: MihomoEndpoint) -> MihomoResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { endpoint, http })
    }

    pub fn endpoint(&self) -> &MihomoEndpoint {
        &self.endpoint
    }

    pub async fn version(&self) -> MihomoResult<VersionInfo> {
        self.get_json("/version").await
    }

    pub async fn configs(&self) -> MihomoResult<Value> {
        self.get_json("/configs").await
    }

    pub async fn runtime_config(&self) -> MihomoResult<RuntimeConfig> {
        self.get_json("/configs").await
    }

    pub async fn proxies(&self) -> MihomoResult<Value> {
        self.get_json("/proxies").await
    }

    pub async fn proxy_catalog(&self) -> MihomoResult<ProxyCatalog> {
        let raw: RawProxyCatalog = self.get_json("/proxies").await?;
        Ok(raw.into())
    }

    pub async fn change_proxy(&self, group: &str, proxy: &str) -> MihomoResult<()> {
        let path = format!("/proxies/{}", encode_path_segment(group));
        self.put_json(&path, &serde_json::json!({ "name": proxy }))
            .await
    }

    pub async fn proxy_delay(
        &self,
        proxy: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<DelayResult> {
        let path = format!("/proxies/{}/delay", encode_path_segment(proxy));
        let timeout = timeout_ms.to_string();
        let url = test_url.unwrap_or("https://www.gstatic.com/generate_204");
        let response = self
            .request(Method::GET, &path)?
            .query(&[("url", url), ("timeout", timeout.as_str())])
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    pub async fn proxy_providers(&self) -> MihomoResult<Value> {
        self.get_json("/providers/proxies").await
    }

    pub async fn proxy_provider_catalog(&self) -> MihomoResult<ProviderCatalog> {
        self.get_json("/providers/proxies").await
    }

    pub async fn rules(&self) -> MihomoResult<Value> {
        self.get_json("/rules").await
    }

    pub async fn rule_catalog(&self) -> MihomoResult<RuleCatalog> {
        self.get_json("/rules").await
    }

    pub async fn rule_providers(&self) -> MihomoResult<Value> {
        self.get_json("/providers/rules").await
    }

    pub async fn rule_provider_catalog(&self) -> MihomoResult<ProviderCatalog> {
        self.get_json("/providers/rules").await
    }

    pub async fn connections(&self) -> MihomoResult<Value> {
        self.get_json("/connections").await
    }

    pub async fn connections_snapshot(&self) -> MihomoResult<ConnectionsSnapshot> {
        self.get_json("/connections").await
    }

    pub async fn memory(&self) -> MihomoResult<Value> {
        self.get_json("/memory").await
    }

    pub async fn set_mode(&self, mode: &str) -> MihomoResult<()> {
        self.patch_json("/configs", &serde_json::json!({ "mode": mode }))
            .await
    }

    pub async fn patch_configs<T: Serialize + ?Sized>(&self, body: &T) -> MihomoResult<()> {
        self.patch_json("/configs", body).await
    }

    pub async fn reload_config(&self, path: &str, force: bool) -> MihomoResult<()> {
        let payload = std::fs::read_to_string(path)
            .map_err(|error| MihomoError::Process(format!("无法读取待重载配置 {path}：{error}")))?;
        self.reload_payload(payload, force).await
    }

    pub async fn reload_payload(
        &self,
        payload: impl Into<String>,
        force: bool,
    ) -> MihomoResult<()> {
        let response = self
            .request(Method::PUT, "/configs")?
            .query(&[("force", force)])
            .json(&serde_json::json!({ "payload": payload.into() }))
            .send()
            .await?
            .error_for_status()?;
        drop(response);
        Ok(())
    }

    pub async fn close_connection(&self, id: &str) -> MihomoResult<()> {
        let path = format!("/connections/{}", encode_path_segment(id));
        self.send_empty(Method::DELETE, &path).await
    }

    pub async fn update_proxy_provider(&self, provider: &str) -> MihomoResult<()> {
        let path = format!("/providers/proxies/{}", encode_path_segment(provider));
        self.send_empty(Method::PUT, &path).await
    }

    pub async fn update_rule_provider(&self, provider: &str) -> MihomoResult<()> {
        let path = format!("/providers/rules/{}", encode_path_segment(provider));
        self.send_empty(Method::PUT, &path).await
    }

    pub async fn close_all_connections(&self) -> MihomoResult<()> {
        self.send_empty(Method::DELETE, "/connections").await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> MihomoResult<T> {
        let response = self
            .request(Method::GET, path)?
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    pub async fn patch_json<T: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> MihomoResult<()> {
        self.request(Method::PATCH, path)?
            .json(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn put_json<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> MihomoResult<()> {
        self.request(Method::PUT, path)?
            .json(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn send_empty(&self, method: Method, path: &str) -> MihomoResult<()> {
        self.request(method, path)?
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    fn request(&self, method: Method, path: &str) -> MihomoResult<RequestBuilder> {
        let mut request = self.http.request(method, self.endpoint.http_url(path)?);
        if !self.endpoint.secret.is_empty() {
            request = request.bearer_auth(&self.endpoint.secret);
        }
        Ok(request)
    }
}

fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_proxy_names_as_single_path_segments() {
        assert_eq!(
            encode_path_segment("HK/香港 #1"),
            "HK%2F%E9%A6%99%E6%B8%AF%20%231"
        );
    }
}
