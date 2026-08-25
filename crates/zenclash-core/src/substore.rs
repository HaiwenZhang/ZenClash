use std::time::Duration;

use serde::Deserialize;

use crate::{MihomoError, MihomoResult};

/// Named subscription or collection returned by a Sub-Store backend.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubStoreItem {
    /// Stable Sub-Store item name used by download endpoints.
    #[serde(default)]
    pub name: String,
    /// Optional user-facing label; callers may fall back to [`Self::name`].
    #[serde(default)]
    pub display_name: String,
    /// User-defined tags associated with the item.
    #[serde(default)]
    pub tag: Vec<String>,
}

/// Point-in-time health and catalog data from a Sub-Store service.
///
/// Subscription and collection requests are independent: a partial response
/// remains available while [`Self::error`] describes the failed endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubStoreSnapshot {
    /// Whether at least one backend catalog endpoint responded successfully.
    pub connected: bool,
    /// Normalized backend base URL used for API requests.
    pub backend_url: String,
    /// Normalized browser URL for the Sub-Store frontend.
    pub frontend_url: String,
    /// Subscriptions returned by the backend, if available.
    pub subscriptions: Vec<SubStoreItem>,
    /// Collections returned by the backend, if available.
    pub collections: Vec<SubStoreItem>,
    /// Combined endpoint-specific errors from this refresh.
    pub error: Option<String>,
}

/// Cloneable client for the small Sub-Store catalog API used by `ZenClash`.
#[derive(Clone)]
pub struct SubStoreClient {
    backend_url: reqwest::Url,
    frontend_url: reqwest::Url,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct DataResponse<T> {
    data: T,
}

impl SubStoreClient {
    /// Creates a client from `ZenClash` environment overrides or local defaults.
    ///
    /// # Errors
    ///
    /// Returns an error when either URL is not a valid unauthenticated HTTP(S)
    /// endpoint, or when the HTTP client cannot be built.
    pub fn from_env() -> MihomoResult<Self> {
        let backend = std::env::var("ZENCLASH_SUBSTORE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:38324".into());
        let frontend = std::env::var("ZENCLASH_SUBSTORE_FRONTEND_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:14122".into());
        Self::new(backend, frontend)
    }

    /// Creates a client for an existing Sub-Store backend and frontend.
    ///
    /// Base paths are preserved, while credentials, query strings, fragments,
    /// missing hosts, and non-HTTP schemes are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid endpoints or HTTP-client construction.
    pub fn new(
        backend_url: impl Into<String>,
        frontend_url: impl Into<String>,
    ) -> MihomoResult<Self> {
        let backend_url = backend_url.into();
        let frontend_url = frontend_url.into();
        let backend_url = normalize_http_url(&backend_url)?;
        let frontend_url = normalize_http_url(&frontend_url)?;
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

    /// Fetches subscriptions and collections concurrently.
    ///
    /// A failure from one endpoint does not discard data returned by the other.
    pub async fn snapshot(&self) -> SubStoreSnapshot {
        let mut snapshot = SubStoreSnapshot {
            backend_url: display_url(&self.backend_url),
            frontend_url: display_url(&self.frontend_url),
            ..Default::default()
        };
        let (subscriptions, collections) = tokio::join!(
            self.get_items("api/subs"),
            self.get_items("api/collections")
        );
        let mut errors = Vec::new();
        match subscriptions {
            Ok(subscriptions) => {
                snapshot.subscriptions = subscriptions;
                snapshot.connected = true;
            }
            Err(error) => errors.push(format!("读取订阅失败：{error}")),
        }
        match collections {
            Ok(collections) => {
                snapshot.collections = collections;
                snapshot.connected = true;
            }
            Err(error) => errors.push(format!("读取集合失败：{error}")),
        }
        if !errors.is_empty() {
            snapshot.error = Some(errors.join("；"));
        }
        snapshot
    }

    async fn get_items(&self, path: &str) -> MihomoResult<Vec<SubStoreItem>> {
        let endpoint = self.backend_url.join(path).map_err(|error| {
            MihomoError::InvalidEndpoint(format!("{} ({error})", self.backend_url))
        })?;
        let response = self.http.get(endpoint).send().await?.error_for_status()?;
        let response: DataResponse<Vec<SubStoreItem>> = response.json().await?;
        Ok(response.data)
    }
}

fn normalize_http_url(url: &str) -> MihomoResult<reqwest::Url> {
    let original = url.trim();
    let mut url = reqwest::Url::parse(original)
        .map_err(|_| MihomoError::InvalidEndpoint(original.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(MihomoError::InvalidEndpoint(original.to_owned()));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{path}/"));
    Ok(url)
}

fn display_url(url: &reqwest::Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
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
    fn validates_and_normalizes_substore_urls() {
        let client =
            SubStoreClient::new("http://127.0.0.1:38324/", "https://substore.example/ui/").unwrap();
        assert_eq!(display_url(&client.backend_url), "http://127.0.0.1:38324");
        assert_eq!(
            display_url(&client.frontend_url),
            "https://substore.example/ui"
        );
    }

    #[test]
    fn preserves_backend_base_path_when_joining_catalog_routes() {
        let client = SubStoreClient::new(
            "https://substore.example/tenant/",
            "https://substore.example/ui",
        )
        .unwrap();

        assert_eq!(
            client.backend_url.join("api/subs").unwrap().as_str(),
            "https://substore.example/tenant/api/subs"
        );
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_substore_urls() {
        let invalid = [
            "file:///tmp/store",
            "http://",
            "http://user:secret@localhost",
            "http://localhost/?tenant=one",
            "http://localhost/#settings",
            "http://localhost:0",
        ];

        assert!(invalid
            .into_iter()
            .all(|backend| { SubStoreClient::new(backend, "http://localhost:14122").is_err() }));
    }

    #[tokio::test]
    async fn snapshot_preserves_a_successful_endpoint_when_the_other_fails() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let (status, payload) = if request.starts_with("GET /api/subs ") {
                    (
                        "200 OK",
                        r#"{"data":[{"name":"main","displayName":"主订阅"}]}"#,
                    )
                } else {
                    ("500 Internal Server Error", r#"{"message":"offline"}"#)
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                )
                .unwrap();
            }
        });
        let client =
            SubStoreClient::new(format!("http://{address}"), "http://127.0.0.1:14122").unwrap();

        let snapshot = client.snapshot().await;
        server.join().unwrap();

        assert_eq!(
            (
                snapshot.connected,
                snapshot
                    .subscriptions
                    .first()
                    .map(|item| item.display_name.as_str()),
                snapshot.collections.is_empty(),
                snapshot
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("读取集合失败")),
            ),
            (true, Some("主订阅"), true, true)
        );
    }
}
