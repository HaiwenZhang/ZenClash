use serde::{Deserialize, Serialize};

use crate::{MihomoError, MihomoResult};

/// Address and authentication information for Mihomo's external controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MihomoEndpoint {
    pub controller: String,
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
    pub fn new(controller: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            controller: controller.into(),
            secret: secret.into(),
        }
    }

    /// Resolve the initial controller from environment overrides, while retaining
    /// Mihomo's conventional local controller as the zero-configuration default.
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

    pub fn http_url(&self, path: &str) -> MihomoResult<String> {
        let base = self.normalized_http_base()?;
        Ok(format!("{base}/{}", path.trim_start_matches('/')))
    }

    pub fn websocket_url(&self, path: &str) -> MihomoResult<String> {
        let base = self.normalized_http_base()?;
        let base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(MihomoError::InvalidEndpoint(self.controller.clone()));
        };
        Ok(format!("{base}/{}", path.trim_start_matches('/')))
    }

    fn normalized_http_base(&self) -> MihomoResult<String> {
        let controller = self.controller.trim().trim_end_matches('/');
        if controller.is_empty() {
            return Err(MihomoError::InvalidEndpoint(self.controller.clone()));
        }

        if controller.starts_with("http://") || controller.starts_with("https://") {
            Ok(controller.to_owned())
        } else if !controller.contains("://") {
            Ok(format!("http://{controller}"))
        } else {
            Err(MihomoError::InvalidEndpoint(self.controller.clone()))
        }
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
}
