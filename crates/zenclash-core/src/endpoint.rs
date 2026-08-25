use serde::{Deserialize, Serialize};

use crate::{MihomoError, MihomoResult};

/// Address and authentication information for Mihomo's external controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MihomoEndpoint {
    /// HTTP(S) controller base URL or `host:port` shorthand.
    pub controller: String,
    /// Bearer token configured as Mihomo's controller secret.
    #[serde(default)]
    pub secret: String,
}

impl Default for MihomoEndpoint {
    fn default() -> Self {
        Self {
            controller: "http://127.0.0.1:9090".to_owned(),
            secret: String::new(),
        }
    }
}

impl MihomoEndpoint {
    /// Creates an endpoint without performing network I/O.
    #[must_use]
    pub fn new(controller: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            controller: controller.into(),
            secret: secret.into(),
        }
    }

    /// Resolve the initial controller from environment overrides, while retaining
    /// Mihomo's conventional local controller as the zero-configuration default.
    #[must_use]
    pub fn from_env() -> Self {
        let mut endpoint = Self::default();
        if let Ok(controller) = std::env::var("ZENCLASH_CONTROLLER") {
            if !controller.trim().is_empty() {
                endpoint.controller = controller;
            }
        }
        if let Ok(secret) = std::env::var("ZENCLASH_SECRET") {
            endpoint.secret = secret;
        }
        endpoint
    }

    /// Builds a validated HTTP(S) URL below the configured controller path.
    ///
    /// # Errors
    ///
    /// Rejects malformed controllers, unsupported schemes, embedded
    /// credentials, queries and fragments.
    pub fn http_url(&self, path: &str) -> MihomoResult<String> {
        let mut url = self.normalized_http_base()?;
        let base_path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base_path}/{}", path.trim_start_matches('/')));
        Ok(url.to_string())
    }

    /// Builds the equivalent WebSocket URL below the controller path.
    ///
    /// # Errors
    ///
    /// Returns the same endpoint-validation errors as [`Self::http_url`].
    pub fn websocket_url(&self, path: &str) -> MihomoResult<String> {
        let mut url = self.normalized_http_base()?;
        let websocket_scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(websocket_scheme)
            .map_err(|()| MihomoError::InvalidEndpoint(self.controller.clone()))?;
        let base_path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base_path}/{}", path.trim_start_matches('/')));
        Ok(url.to_string())
    }

    fn normalized_http_base(&self) -> MihomoResult<reqwest::Url> {
        let controller = self.controller.trim().trim_end_matches('/');
        if controller.is_empty() {
            return Err(MihomoError::InvalidEndpoint(self.controller.clone()));
        }
        let controller = if controller.starts_with("http://") || controller.starts_with("https://")
        {
            controller.to_owned()
        } else if !controller.contains("://") {
            format!("http://{controller}")
        } else {
            return Err(MihomoError::InvalidEndpoint(self.controller.clone()));
        };
        let url = reqwest::Url::parse(&controller)
            .map_err(|_| MihomoError::InvalidEndpoint(self.controller.clone()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(MihomoError::InvalidEndpoint(self.controller.clone()));
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_http_and_websocket_urls() {
        let endpoint = MihomoEndpoint::new("127.0.0.1:9090/", "");
        assert_eq!(
            endpoint.http_url("/version").unwrap(),
            "http://127.0.0.1:9090/version"
        );
        assert_eq!(
            endpoint.websocket_url("traffic").unwrap(),
            "ws://127.0.0.1:9090/traffic"
        );

        let secure = MihomoEndpoint::new("https://controller.example", "secret");
        assert_eq!(
            secure.websocket_url("/traffic").unwrap(),
            "wss://controller.example/traffic"
        );
    }

    #[test]
    fn rejects_non_http_controller_schemes() {
        let endpoint = MihomoEndpoint::new("unix:///tmp/mihomo.sock", "");
        assert!(matches!(
            endpoint.http_url("version"),
            Err(MihomoError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn rejects_controller_credentials_and_query_parameters() {
        let with_credentials = MihomoEndpoint::new("http://user:pass@127.0.0.1:9090", "");
        let with_query = MihomoEndpoint::new("http://127.0.0.1:9090?token=secret", "");

        assert_eq!(
            (
                with_credentials.http_url("version").is_err(),
                with_query.http_url("version").is_err()
            ),
            (true, true)
        );
    }

    #[test]
    fn preserves_controller_base_path_without_treating_request_path_as_query() {
        let endpoint = MihomoEndpoint::new("https://controller.example/api", "");
        assert_eq!(
            endpoint.http_url("version?raw=true").unwrap(),
            "https://controller.example/api/version%3Fraw=true"
        );
    }

    #[test]
    fn preserves_percent_encoded_mihomo_path_segments() {
        let endpoint = MihomoEndpoint::new("http://127.0.0.1:9090", "");
        assert_eq!(
            endpoint
                .http_url("/proxies/HK%2F%E9%A6%99%E6%B8%AF")
                .unwrap(),
            "http://127.0.0.1:9090/proxies/HK%2F%E9%A6%99%E6%B8%AF"
        );
    }
}
