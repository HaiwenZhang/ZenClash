//! Last-known-good state for provider update and health-check operations.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use thiserror::Error;

use crate::{MihomoClient, Provider, ProviderCatalog};

/// Kind of Mihomo provider being observed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProviderKind {
    /// Provider containing proxy nodes.
    Proxy,
    /// Provider containing routing rules.
    Rule,
}

/// Failure retained for one provider operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderOperationFailure {
    /// Human-readable interactive error.
    pub message: String,
    /// Unix timestamp in milliseconds when the operation failed.
    pub occurred_at_ms: u64,
}

/// Independent history for update or health-check actions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderActionStatus {
    /// Most recent successful completion time.
    pub last_success_at_ms: Option<u64>,
    /// Most recent failure, retained after a later success for diagnostics.
    pub last_failure: Option<ProviderOperationFailure>,
}

/// Last-known-good provider data and its operation history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderOperationalStatus {
    /// Provider kind.
    pub kind: ProviderKind,
    /// Stable provider name used by controller operations.
    pub name: String,
    /// Most recent successful catalog or operation observation.
    pub last_success_at_ms: Option<u64>,
    /// Most recent catalog or operation failure.
    pub last_failure: Option<ProviderOperationFailure>,
    /// Proxy or rule count from the last-known-good provider value.
    pub item_count: usize,
    /// Provider value retained across later failures.
    pub last_known_good: Option<Provider>,
    /// Update/download action history.
    pub update: ProviderActionStatus,
    /// Health-check action history, used only by proxy providers.
    pub healthcheck: ProviderActionStatus,
}

impl ProviderOperationalStatus {
    fn empty(kind: ProviderKind, name: String) -> Self {
        Self {
            kind,
            name,
            last_success_at_ms: None,
            last_failure: None,
            item_count: 0,
            last_known_good: None,
            update: ProviderActionStatus::default(),
            healthcheck: ProviderActionStatus::default(),
        }
    }
}

/// Errors from a provider operation or its required readback.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderOperationError {
    /// Mihomo rejected or failed the requested operation.
    #[error("Provider 操作失败：{0}")]
    Operation(String),
    /// A successful catalog did not contain the requested provider.
    #[error("Provider 回读缺少 {0}")]
    MissingReadback(String),
}

/// Result type for provider operations.
pub type ProviderOperationResult<T> = Result<T, ProviderOperationError>;

/// Coordinates provider actions while retaining independent last-known-good state.
#[derive(Clone)]
pub struct ProviderOperations {
    client: MihomoClient,
    states: Arc<RwLock<BTreeMap<(ProviderKind, String), ProviderOperationalStatus>>>,
}

impl ProviderOperations {
    /// Creates an operation owner for one Mihomo controller client.
    #[must_use]
    pub fn new(client: MihomoClient) -> Self {
        Self {
            client,
            states: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Records a successful catalog without discarding earlier action history.
    pub fn observe_catalog(&self, kind: ProviderKind, catalog: &ProviderCatalog) {
        let observed_at_ms = now_ms();
        let mut states = self.states.write();
        for (key, provider) in &catalog.providers {
            let name = provider_name(key, provider);
            let state = states
                .entry((kind, name.clone()))
                .or_insert_with(|| ProviderOperationalStatus::empty(kind, name));
            accept_provider(state, provider.clone(), observed_at_ms);
        }
    }

    /// Returns the retained state for one provider.
    #[must_use]
    pub fn status(&self, kind: ProviderKind, name: &str) -> Option<ProviderOperationalStatus> {
        self.states.read().get(&(kind, name.to_owned())).cloned()
    }

    /// Downloads a fresh provider document and verifies it through catalog readback.
    ///
    /// A failed update records its own failure while preserving the previous
    /// provider value and count.
    ///
    /// # Errors
    ///
    /// Returns controller or readback errors.
    pub async fn update(
        &self,
        kind: ProviderKind,
        name: &str,
    ) -> ProviderOperationResult<ProviderCatalog> {
        let result = async {
            match kind {
                ProviderKind::Proxy => self.client.update_proxy_provider(name).await,
                ProviderKind::Rule => self.client.update_rule_provider(name).await,
            }
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
            self.catalog(kind)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))
        }
        .await;
        self.finish_action(kind, name, ProviderAction::Update, result)
    }

    /// Runs a proxy-provider health check without downloading a new document.
    ///
    /// Update and health-check histories remain independent.
    ///
    /// # Errors
    ///
    /// Returns controller or readback errors.
    pub async fn healthcheck_proxy(&self, name: &str) -> ProviderOperationResult<ProviderCatalog> {
        let result = async {
            self.client
                .healthcheck_proxy_provider(name)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
            self.catalog(ProviderKind::Proxy)
                .await
                .map_err(|error| ProviderOperationError::Operation(error.to_string()))
        }
        .await;
        self.finish_action(
            ProviderKind::Proxy,
            name,
            ProviderAction::Healthcheck,
            result,
        )
    }

    async fn catalog(&self, kind: ProviderKind) -> crate::MihomoResult<ProviderCatalog> {
        match kind {
            ProviderKind::Proxy => self.client.proxy_provider_catalog().await,
            ProviderKind::Rule => self.client.rule_provider_catalog().await,
        }
    }

    fn finish_action(
        &self,
        kind: ProviderKind,
        name: &str,
        action: ProviderAction,
        result: ProviderOperationResult<ProviderCatalog>,
    ) -> ProviderOperationResult<ProviderCatalog> {
        match result {
            Ok(catalog) => {
                let provider = find_provider(&catalog, name)
                    .cloned()
                    .ok_or_else(|| ProviderOperationError::MissingReadback(name.to_owned()));
                match provider {
                    Ok(provider) => {
                        let observed_at_ms = now_ms();
                        let mut states = self.states.write();
                        let state = states.entry((kind, name.to_owned())).or_insert_with(|| {
                            ProviderOperationalStatus::empty(kind, name.to_owned())
                        });
                        accept_provider(state, provider, observed_at_ms);
                        action.status_mut(state).last_success_at_ms = Some(observed_at_ms);
                        Ok(catalog)
                    }
                    Err(error) => {
                        self.record_failure(kind, name, action, &error);
                        Err(error)
                    }
                }
            }
            Err(error) => {
                self.record_failure(kind, name, action, &error);
                Err(error)
            }
        }
    }

    fn record_failure(
        &self,
        kind: ProviderKind,
        name: &str,
        action: ProviderAction,
        error: &ProviderOperationError,
    ) {
        let failure = ProviderOperationFailure {
            message: error.to_string(),
            occurred_at_ms: now_ms(),
        };
        let mut states = self.states.write();
        let state = states
            .entry((kind, name.to_owned()))
            .or_insert_with(|| ProviderOperationalStatus::empty(kind, name.to_owned()));
        state.last_failure = Some(failure.clone());
        action.status_mut(state).last_failure = Some(failure);
    }
}

#[derive(Clone, Copy)]
enum ProviderAction {
    Update,
    Healthcheck,
}

impl ProviderAction {
    fn status_mut(self, state: &mut ProviderOperationalStatus) -> &mut ProviderActionStatus {
        match self {
            Self::Update => &mut state.update,
            Self::Healthcheck => &mut state.healthcheck,
        }
    }
}

fn find_provider<'a>(catalog: &'a ProviderCatalog, name: &str) -> Option<&'a Provider> {
    catalog.providers.get(name).or_else(|| {
        catalog
            .providers
            .values()
            .find(|provider| provider.name == name)
    })
}

fn provider_name(key: &str, provider: &Provider) -> String {
    if provider.name.trim().is_empty() {
        key.to_owned()
    } else {
        provider.name.clone()
    }
}

fn accept_provider(state: &mut ProviderOperationalStatus, provider: Provider, observed_at_ms: u64) {
    state.item_count = match state.kind {
        ProviderKind::Proxy => provider.proxies.len(),
        ProviderKind::Rule => provider.rule_count,
    };
    state.last_known_good = Some(provider);
    state.last_success_at_ms = Some(observed_at_ms);
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;
    use crate::MihomoEndpoint;

    fn proxy_catalog(count: usize) -> ProviderCatalog {
        ProviderCatalog {
            providers: BTreeMap::from([(
                "airport".into(),
                Provider {
                    name: "airport".into(),
                    proxies: (0..count).map(serde_json::Value::from).collect(),
                    ..Provider::default()
                },
            )]),
        }
    }

    #[tokio::test]
    async fn failed_update_keeps_last_known_good_provider_and_count() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"message":"download failed"}"#;
            write!(
                stream,
                "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let operations = ProviderOperations::new(client);
        operations.observe_catalog(ProviderKind::Proxy, &proxy_catalog(3));

        let result = operations.update(ProviderKind::Proxy, "airport").await;
        server.join().unwrap();

        assert!(result.is_err());
        let status = operations.status(ProviderKind::Proxy, "airport").unwrap();
        assert_eq!(status.item_count, 3);
        assert_eq!(status.last_known_good.unwrap().proxies.len(), 3);
        assert!(status.last_success_at_ms.is_some());
        assert!(status.last_failure.is_some());
        assert!(status.update.last_failure.is_some());
        assert!(status.healthcheck.last_failure.is_none());
    }

    #[tokio::test]
    async fn healthcheck_has_independent_success_and_readback() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut first_lines = Vec::new();
            for body in [
                None,
                Some(r#"{"providers":{"airport":{"name":"airport","proxies":[{},{}]}}}"#),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let bytes = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..bytes]);
                first_lines.push(request.lines().next().unwrap_or_default().to_owned());
                if let Some(body) = body {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            }
            first_lines
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let operations = ProviderOperations::new(client);

        operations.healthcheck_proxy("airport").await.unwrap();
        let requests = server.join().unwrap();

        assert_eq!(
            requests,
            [
                "GET /providers/proxies/airport/healthcheck HTTP/1.1",
                "GET /providers/proxies HTTP/1.1",
            ]
        );
        let status = operations.status(ProviderKind::Proxy, "airport").unwrap();
        assert_eq!(status.item_count, 2);
        assert!(status.healthcheck.last_success_at_ms.is_some());
        assert!(status.update.last_success_at_ms.is_none());
    }
}
