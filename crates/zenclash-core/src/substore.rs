use std::time::Duration;

use serde::Deserialize;

use crate::{MihomoError, MihomoResult};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubStoreItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub tag: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubStoreSnapshot {
    pub connected: bool,
    pub backend_url: String,
    pub frontend_url: String,
    pub subscriptions: Vec<SubStoreItem>,
    pub collections: Vec<SubStoreItem>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct SubStoreClient {
    backend_url: String,
    frontend_url: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct DataResponse<T> {
    data: T,
}

impl SubStoreClient {
    pub fn from_env() -> MihomoResult<Self> {
        let backend = std::env::var("ZENCLASH_SUBSTORE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:38324".into());
        let frontend = std::env::var("ZENCLASH_SUBSTORE_FRONTEND_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:14122".into());
        Self::new(backend, frontend)
    }

    pub fn new(
        backend_url: impl Into<String>,
        frontend_url: impl Into<String>,
    ) -> MihomoResult<Self> {
        let backend_url = normalize_http_url(backend_url.into())?;
        let frontend_url = normalize_http_url(frontend_url.into())?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(2))
            .build()?;
        Ok(Self {
            backend_url,
            frontend_url,
            http,
        })
    }

    pub async fn snapshot(&self) -> SubStoreSnapshot {
        let mut snapshot = SubStoreSnapshot {
            backend_url: self.backend_url.clone(),
            frontend_url: self.frontend_url.clone(),
            ..Default::default()
        };
        let subscriptions = self.get_items("/api/subs").await;
        let collections = self.get_items("/api/collections").await;
        match (subscriptions, collections) {
            (Ok(subscriptions), Ok(collections)) => {
                snapshot.connected = true;
                snapshot.subscriptions = subscriptions;
                snapshot.collections = collections;
            }
            (subscriptions, collections) => {
                let errors = [subscriptions.err(), collections.err()]
                    .into_iter()
                    .flatten()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>();
                snapshot.error = Some(errors.join("；"));
            }
        }
        snapshot
    }

    async fn get_items(&self, path: &str) -> MihomoResult<Vec<SubStoreItem>> {
        let response = self
            .http
            .get(format!("{}{}", self.backend_url, path))
            .send()
            .await?
            .error_for_status()?;
        let response: DataResponse<Vec<SubStoreItem>> = response.json().await?;
        Ok(response.data)
    }
}

fn normalize_http_url(url: String) -> MihomoResult<String> {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url.into())
    } else {
        Err(MihomoError::InvalidEndpoint(url.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_normalizes_substore_urls() {
        let client =
            SubStoreClient::new("http://127.0.0.1:38324/", "https://substore.example/ui/").unwrap();
        assert_eq!(client.backend_url, "http://127.0.0.1:38324");
        assert_eq!(client.frontend_url, "https://substore.example/ui");
        assert!(SubStoreClient::new("file:///tmp/store", "http://localhost").is_err());
    }
}
