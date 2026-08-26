use std::{path::Path, time::Duration};

use reqwest::Method;
use serde::Serialize;

use super::{request, MihomoClient, MihomoError, MihomoResult, VersionInfo};
use crate::{
    profiles::{read_profile_bytes, MAX_PROFILE_BYTES},
    proxy::RawProxyCatalog,
    ConnectionsSnapshot, DelayResult, ProviderCatalog, ProxyCatalog, RuleCatalog, RuntimeConfig,
};

impl MihomoClient {
    /// Fetches Mihomo core version information.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn version(&self) -> MihomoResult<VersionInfo> {
        self.get_json("/version").await
    }

    /// Fetches the current typed runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn runtime_config(&self) -> MihomoResult<RuntimeConfig> {
        self.get_json("/configs").await
    }

    /// Fetches and resolves proxy nodes and selector groups.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn proxy_catalog(&self) -> MihomoResult<ProxyCatalog> {
        let raw: RawProxyCatalog = self.get_json("/proxies").await?;
        Ok(raw.into())
    }

    /// Selects a proxy node for a Mihomo selector group.
    ///
    /// # Errors
    ///
    /// Rejects empty names and propagates transport or API-status errors.
    pub async fn change_proxy(&self, group: &str, proxy: &str) -> MihomoResult<()> {
        require_non_empty(group, "代理组名称")?;
        require_non_empty(proxy, "代理节点名称")?;
        let path = format!("/proxies/{}", encode_path_segment(group));
        self.put_json(&path, &serde_json::json!({ "name": proxy }))
            .await
    }

    /// Measures one proxy's delay through Mihomo.
    ///
    /// # Errors
    ///
    /// Rejects empty proxy names, zero timeouts and non-HTTP(S) test URLs, and
    /// propagates transport, API-status or response-decoding errors.
    pub async fn proxy_delay(
        &self,
        proxy: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<DelayResult> {
        require_non_empty(proxy, "代理节点名称")?;
        if timeout_ms == 0 {
            return Err(MihomoError::InvalidInput("延迟测试超时必须大于 0".into()));
        }
        let path = format!("/proxies/{}/delay", encode_path_segment(proxy));
        let timeout = timeout_ms.to_string();
        let url = validated_test_url(test_url)?;
        let response = self
            .request(Method::GET, &path)?
            .query(&[("url", url.as_str()), ("timeout", timeout.as_str())])
            .send()
            .await?;
        let response = request::ensure_success(response).await?;
        Ok(response.json().await?)
    }

    /// Fetches proxy-provider metadata.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn proxy_provider_catalog(&self) -> MihomoResult<ProviderCatalog> {
        self.get_json("/providers/proxies").await
    }

    /// Fetches the active ruleset.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn rule_catalog(&self) -> MihomoResult<RuleCatalog> {
        self.get_json("/rules").await
    }

    /// Fetches rule-provider metadata.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn rule_provider_catalog(&self) -> MihomoResult<ProviderCatalog> {
        self.get_json("/providers/rules").await
    }

    /// Fetches the current connection snapshot.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status or response-decoding errors.
    pub async fn connections_snapshot(&self) -> MihomoResult<ConnectionsSnapshot> {
        self.get_json("/connections").await
    }

    /// Changes Mihomo's outbound mode to `rule`, `global` or `direct`.
    ///
    /// # Errors
    ///
    /// Rejects unsupported modes and propagates transport or API-status errors.
    pub async fn set_mode(&self, mode: &str) -> MihomoResult<()> {
        let mode = mode.trim().to_ascii_lowercase();
        if !matches!(mode.as_str(), "rule" | "global" | "direct") {
            return Err(MihomoError::InvalidInput(format!(
                "不支持的出站模式：{mode}"
            )));
        }
        self.patch_configs_verified(&serde_json::json!({ "mode": mode }))
            .await
            .map(|_| ())
    }

    /// Applies a partial typed or JSON-compatible `/configs` update.
    ///
    /// # Errors
    ///
    /// Returns serialization, transport or API-status errors.
    pub async fn patch_configs<T: Serialize + Sync + ?Sized>(&self, body: &T) -> MihomoResult<()> {
        self.patch_json("/configs", body).await
    }

    /// Applies a JSON `/configs` patch and verifies that Mihomo reports the
    /// requested values afterward.
    ///
    /// Mihomo returns success for some unsupported runtime fields while
    /// silently keeping the previous configuration. UI controls should use
    /// this method so an accepted HTTP response is not presented as an applied
    /// setting.
    ///
    /// # Errors
    ///
    /// Returns an error when the patch is not an object, the request or
    /// readback fails, or the returned runtime configuration does not contain
    /// the requested values.
    pub async fn patch_configs_verified(
        &self,
        body: &serde_json::Value,
    ) -> MihomoResult<RuntimeConfig> {
        if !body.is_object() {
            return Err(MihomoError::InvalidInput(
                "运行时配置补丁必须是 JSON 对象".into(),
            ));
        }
        self.patch_configs(body).await?;
        let config = self.runtime_config().await?;
        let actual = serde_json::to_value(&config).map_err(|error| {
            MihomoError::Process(format!("无法验证 Mihomo 运行时配置：{error}"))
        })?;
        if !json_contains(&actual, body) {
            return Err(MihomoError::Process(format!(
                "Mihomo 接受了配置请求，但状态回读未应用：{body}"
            )));
        }
        Ok(config)
    }

    /// Reads a UTF-8 YAML file asynchronously and asks Mihomo to reload it.
    ///
    /// # Errors
    ///
    /// Returns filesystem, validation, transport or API-status errors.
    pub async fn reload_config(&self, path: impl AsRef<Path>, force: bool) -> MihomoResult<()> {
        let path = path.as_ref().to_path_buf();
        let display_path = path.display().to_string();
        let payload = tokio::task::spawn_blocking(move || read_profile_bytes(&path))
            .await
            .map_err(|error| {
                MihomoError::Process(format!("读取待重载配置的后台任务异常结束：{error}"))
            })?
            .map_err(|error| {
                MihomoError::Process(format!("无法读取待重载配置 {display_path}：{error}"))
            })?;
        let payload = String::from_utf8(payload)
            .map_err(|error| MihomoError::InvalidInput(format!("待重载配置不是 UTF-8：{error}")))?;
        self.reload_payload(payload, force).await
    }

    /// Asks Mihomo to reload an in-memory YAML payload.
    ///
    /// # Errors
    ///
    /// Rejects empty payloads and propagates transport or API-status errors.
    pub async fn reload_payload(
        &self,
        payload: impl Into<String>,
        force: bool,
    ) -> MihomoResult<()> {
        let payload = payload.into();
        if payload.trim().is_empty() {
            return Err(MihomoError::InvalidInput("重载配置内容不能为空".into()));
        }
        if payload.len() > MAX_PROFILE_BYTES {
            return Err(MihomoError::InvalidInput(format!(
                "重载配置超过 {} MiB 限制",
                MAX_PROFILE_BYTES / 1024 / 1024
            )));
        }
        let _mutation_guard = self.mutation_gate.lock().await;
        let response = self
            .request(Method::PUT, "/configs")?
            .query(&[("force", force)])
            .json(&serde_json::json!({ "payload": payload }))
            .send()
            .await?;
        request::ensure_success(response).await?;
        Ok(())
    }

    /// Closes one connection by ID.
    ///
    /// # Errors
    ///
    /// Rejects an empty ID and propagates transport or API-status errors.
    pub async fn close_connection(&self, id: &str) -> MihomoResult<()> {
        require_non_empty(id, "连接 ID")?;
        let path = format!("/connections/{}", encode_path_segment(id));
        self.send_empty(Method::DELETE, &path).await
    }

    /// Requests a proxy-provider refresh.
    ///
    /// # Errors
    ///
    /// Rejects an empty name and propagates transport or API-status errors.
    pub async fn update_proxy_provider(&self, provider: &str) -> MihomoResult<()> {
        require_non_empty(provider, "代理 Provider 名称")?;
        let path = format!("/providers/proxies/{}", encode_path_segment(provider));
        self.send_empty(Method::PUT, &path).await
    }

    /// Requests a rule-provider refresh.
    ///
    /// # Errors
    ///
    /// Rejects an empty name and propagates transport or API-status errors.
    pub async fn update_rule_provider(&self, provider: &str) -> MihomoResult<()> {
        require_non_empty(provider, "规则 Provider 名称")?;
        let path = format!("/providers/rules/{}", encode_path_segment(provider));
        self.send_empty(Method::PUT, &path).await
    }

    /// Closes every active Mihomo connection.
    ///
    /// # Errors
    ///
    /// Returns transport or API-status errors.
    pub async fn close_all_connections(&self) -> MihomoResult<()> {
        self.send_empty(Method::DELETE, "/connections").await
    }

    /// Asks the running Mihomo core to download and install its latest release.
    ///
    /// The request uses a longer timeout because Mihomo performs the download
    /// and replacement before acknowledging the operation.
    ///
    /// # Errors
    ///
    /// Returns transport or API-status errors reported by Mihomo.
    pub async fn upgrade_core(&self) -> MihomoResult<()> {
        self.send_long_operation(Method::POST, "/upgrade").await
    }

    /// Asks Mihomo to refresh `GeoIP`, `GeoSite` and MMDB assets from `geox-url`.
    ///
    /// # Errors
    ///
    /// Returns transport or API-status errors reported by Mihomo.
    pub async fn update_geodata(&self) -> MihomoResult<()> {
        self.send_long_operation(Method::POST, "/configs/geo").await
    }

    /// Asks Mihomo to refresh the configured external Web UI archive.
    ///
    /// # Errors
    ///
    /// Returns transport or API-status errors reported by Mihomo.
    pub async fn update_external_ui(&self) -> MihomoResult<()> {
        self.send_long_operation(Method::POST, "/upgrade/ui").await
    }

    /// Enables or disables one compiled rule by its runtime index and verifies
    /// the state through a fresh `/rules` response.
    ///
    /// # Errors
    ///
    /// Returns transport or API-status errors, or a verification error when
    /// the running Mihomo build does not expose the requested indexed rule and
    /// its mutable runtime state.
    pub async fn set_rule_disabled(
        &self,
        index: usize,
        disabled: bool,
    ) -> MihomoResult<RuleCatalog> {
        let _mutation_guard = self.mutation_gate.lock().await;
        let body = serde_json::json!({index.to_string(): disabled});
        let response = self
            .request(Method::PATCH, "/rules/disable")?
            .json(&body)
            .send()
            .await?;
        request::ensure_success(response).await?;
        let catalog = self.rule_catalog().await?;
        let verified = catalog.rules.iter().any(|rule| {
            rule.index == Some(index)
                && rule
                    .extra
                    .as_ref()
                    .is_some_and(|extra| extra.disabled == disabled)
        });
        if !verified {
            return Err(MihomoError::Process(format!(
                "Mihomo 接受了规则状态请求，但规则 #{index} 的回读状态不匹配"
            )));
        }
        Ok(catalog)
    }

    async fn send_long_operation(&self, method: Method, path: &str) -> MihomoResult<()> {
        let _mutation_guard = self.mutation_gate.lock().await;
        let response = self
            .request(method, path)?
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        request::ensure_success(response).await?;
        Ok(())
    }
}

fn require_non_empty(value: &str, label: &str) -> MihomoResult<()> {
    if value.trim().is_empty() {
        Err(MihomoError::InvalidInput(format!("{label}不能为空")))
    } else {
        Ok(())
    }
}

fn json_contains(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    match expected {
        serde_json::Value::Object(expected) => actual.as_object().is_some_and(|actual| {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| json_contains(actual, expected))
            })
        }),
        _ => actual == expected,
    }
}

fn validated_test_url(value: Option<&str>) -> MihomoResult<reqwest::Url> {
    let value = value.unwrap_or("https://www.gstatic.com/generate_204");
    let url = reqwest::Url::parse(value)
        .map_err(|error| MihomoError::InvalidInput(format!("延迟测试 URL 无效：{error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(MihomoError::InvalidInput(
            "延迟测试 URL 仅支持 HTTP 或 HTTPS".into(),
        ));
    }
    Ok(url)
}

pub(super) fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod verification_tests {
    use super::json_contains;

    #[test]
    fn matches_nested_runtime_patch_as_a_subset() {
        let actual = serde_json::json!({
            "ipv6": true,
            "tun": {"enable": false, "stack": "mixed"}
        });

        assert!(json_contains(
            &actual,
            &serde_json::json!({"tun": {"enable": false}})
        ));
        assert!(!json_contains(
            &actual,
            &serde_json::json!({"tun": {"enable": true}})
        ));
    }
}
