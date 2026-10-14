//! Last-known-good state for provider update and health-check operations.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use thiserror::Error;

use crate::{MihomoClient, Provider, ProviderCatalog};

mod action_receipt;
mod actions;
pub use action_receipt::ProviderActionReceipt;

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

/// One controller-key target in a provider update batch.
#[derive(Clone, Debug)]
pub struct ProviderUpdateTarget {
    kind: ProviderKind,
    controller_key: String,
}

impl ProviderUpdateTarget {
    /// Creates a target using the provider collection kind and controller map key.
    #[must_use]
    pub fn new(kind: ProviderKind, controller_key: impl Into<String>) -> Self {
        Self {
            kind,
            controller_key: controller_key.into(),
        }
    }
}

/// Batch diagnostics and the original runtime client for subsequent readback.
pub struct ProviderUpdateBatchReceipt {
    client: MihomoClient,
    failures: Vec<String>,
}

impl ProviderUpdateBatchReceipt {
    /// Returns the batch's pinned runtime client for its presentation readback.
    #[must_use]
    pub fn client(&self) -> &MihomoClient {
        &self.client
    }
    /// Returns per-target update/readback failures without losing successful history.
    #[must_use]
    pub fn failures(&self) -> &[String] {
        &self.failures
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
    observations: Arc<RwLock<ProviderObservationLedger>>,
}

#[derive(Default)]
struct ProviderObservationLedger {
    binding_generation: Option<u64>,
    states: BTreeMap<(ProviderKind, String), ProviderOperationalStatus>,
}

impl ProviderObservationLedger {
    fn for_binding(
        &mut self,
        generation: u64,
    ) -> &mut BTreeMap<(ProviderKind, String), ProviderOperationalStatus> {
        if self.binding_generation != Some(generation) {
            self.states.clear();
            self.binding_generation = Some(generation);
        }
        &mut self.states
    }
}

impl ProviderOperations {
    pub(crate) fn with_admitted_client(
        &self,
        client: MihomoClient,
    ) -> ProviderOperationResult<Self> {
        self.client
            .ensure_binding_current()
            .map_err(|error| ProviderOperationError::Operation(error.to_string()))?;
        if !Arc::ptr_eq(&self.client.binding, &client.binding) {
            return Err(ProviderOperationError::Operation(
                crate::MihomoError::StaleBinding.to_string(),
            ));
        }
        Ok(Self {
            client,
            observations: self.observations.clone(),
        })
    }
    /// Creates an operation owner for one Mihomo controller client.
    #[must_use]
    pub fn new(client: MihomoClient) -> Self {
        Self {
            client,
            observations: Arc::default(),
        }
    }

    /// Records a successful catalog without discarding earlier action history.
    pub fn observe_catalog(&self, kind: ProviderKind, catalog: &ProviderCatalog) {
        let _ = self
            .client
            .with_current_binding(|generation| self.record_catalog(generation, kind, catalog));
    }

    /// Records page-read provider facts only for the runtime identity that produced them.
    /// Returns false after a configuration/owner change or during an admitted transition.
    pub fn observe_catalog_for_runtime(
        &self,
        kind: ProviderKind,
        catalog: &ProviderCatalog,
        identity: &crate::CoreObservationIdentity,
    ) -> bool {
        identity
            .publish_for_client(&self.client, |generation| {
                self.record_catalog(generation, kind, catalog)
            })
            .is_some()
    }

    fn record_catalog(&self, generation: u64, kind: ProviderKind, catalog: &ProviderCatalog) {
        let observed_at_ms = now_ms();
        let mut observations = self.observations.write();
        let states = observations.for_binding(generation);
        for (key, provider) in &catalog.providers {
            let name = key.clone();
            let state = states
                .entry((kind, name.clone()))
                .or_insert_with(|| ProviderOperationalStatus::empty(kind, name));
            accept_provider(state, provider.clone(), observed_at_ms);
        }
    }

    /// Returns the retained state for one provider.
    #[must_use]
    pub fn status(&self, kind: ProviderKind, name: &str) -> Option<ProviderOperationalStatus> {
        self.client
            .with_current_binding(|generation| {
                let observations = self.observations.read();
                (observations.binding_generation == Some(generation))
                    .then(|| observations.states.get(&(kind, name.to_owned())).cloned())
                    .flatten()
            })
            .ok()
            .flatten()
    }

    async fn catalog_on(
        client: &MihomoClient,
        kind: ProviderKind,
    ) -> crate::MihomoResult<ProviderCatalog> {
        match kind {
            ProviderKind::Proxy => client.proxy_provider_catalog().await,
            ProviderKind::Rule => client.rule_provider_catalog().await,
        }
    }

    fn finish_action(
        &self,
        generation: u64,
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
                        let mut observations = self.observations.write();
                        let states = observations.for_binding(generation);
                        let state = states.entry((kind, name.to_owned())).or_insert_with(|| {
                            ProviderOperationalStatus::empty(kind, name.to_owned())
                        });
                        accept_provider(state, provider, observed_at_ms);
                        action.status_mut(state).last_success_at_ms = Some(observed_at_ms);
                        Ok(catalog)
                    }
                    Err(error) => {
                        self.record_failure(generation, kind, name, action, &error);
                        Err(error)
                    }
                }
            }
            Err(error) => {
                self.record_failure(generation, kind, name, action, &error);
                Err(error)
            }
        }
    }

    fn record_failure(
        &self,
        generation: u64,
        kind: ProviderKind,
        name: &str,
        action: ProviderAction,
        error: &ProviderOperationError,
    ) {
        let failure = ProviderOperationFailure {
            message: error.to_string(),
            occurred_at_ms: now_ms(),
        };
        let mut observations = self.observations.write();
        let states = observations.for_binding(generation);
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

fn find_provider<'a>(catalog: &'a ProviderCatalog, controller_key: &str) -> Option<&'a Provider> {
    catalog.providers.get(controller_key)
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
        future::Future,
        io::{Read, Write},
        net::TcpListener,
        pin::Pin,
        task::{Context, Poll, Wake, Waker},
        thread,
        time::Duration,
    };

    use super::*;
    use crate::MihomoEndpoint;

    type SwitchFuture = Pin<Box<dyn Future<Output = crate::MihomoResult<()>> + Send>>;

    struct SwitchOnUnlock(parking_lot::Mutex<Option<SwitchFuture>>);

    impl Wake for SwitchOnUnlock {
        fn wake(self: Arc<Self>) {
            let mut future = self.0.lock();
            if let Some(switch) = future.as_mut()
                && let Poll::Ready(result) = switch
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
            {
                result.unwrap();
                *future = None;
            }
        }
    }

    fn queue_runtime_switch(client: MihomoClient, endpoint: MihomoEndpoint) -> Arc<SwitchOnUnlock> {
        let switch = Arc::new(SwitchOnUnlock(parking_lot::Mutex::new(Some(Box::pin(
            async move { client.switch_to_direct(endpoint).await },
        )))));
        let waker = Waker::from(switch.clone());
        // Publish synchronously when the PUT releases admission, before its catalog readback.
        assert!(
            switch
                .0
                .lock()
                .as_mut()
                .unwrap()
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        switch
    }

    async fn finish_runtime_switch(switch: Arc<SwitchOnUnlock>) {
        let retirement = switch.0.lock().take();
        if let Some(retirement) = retirement {
            tokio::time::timeout(Duration::from_secs(2), retirement)
                .await
                .unwrap()
                .unwrap();
        }
    }

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

    #[test]
    fn duplicate_provider_labels_keep_independent_controller_keys() {
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
        let operations = ProviderOperations::new(client);
        let provider = Provider {
            name: "Visible label".into(),
            ..Provider::default()
        };
        let catalog = ProviderCatalog {
            providers: BTreeMap::from([
                ("first-key".into(), provider.clone()),
                ("second-key".into(), provider),
            ]),
        };
        operations.observe_catalog(ProviderKind::Proxy, &catalog);
        assert_eq!(
            operations
                .status(ProviderKind::Proxy, "first-key")
                .unwrap()
                .name,
            "first-key"
        );
        assert_eq!(
            operations
                .status(ProviderKind::Proxy, "second-key")
                .unwrap()
                .name,
            "second-key"
        );
        assert!(
            operations
                .status(ProviderKind::Proxy, "Visible label")
                .is_none()
        );
    }

    #[tokio::test]
    async fn provider_update_readback_and_history_remain_bound_to_the_original_runtime() {
        use tokio::io::AsyncWriteExt;
        let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", original.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
        let operations = ProviderOperations::new(client.clone());
        operations.observe_catalog(ProviderKind::Proxy, &proxy_catalog(3));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = original.accept().await.unwrap();
            let request = crate::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            assert!(
                String::from_utf8_lossy(&request).starts_with("PUT /providers/proxies/airport ")
            );
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), original.accept())
                    .await
                    .is_err(),
                "provider readback or remaining targets reached the retired controller"
            );
        });
        let updating_operations = operations.clone();
        let updating = tokio::spawn(async move {
            updating_operations
                .update(ProviderKind::Proxy, "airport")
                .await
        });
        entered_rx.await.unwrap();
        let switching = queue_runtime_switch(client, MihomoEndpoint::new(replacement_url, ""));
        release_tx.send(()).unwrap();
        server.await.unwrap();
        finish_runtime_switch(switching).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), updating)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(operations.status(ProviderKind::Proxy, "airport").is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn batch_stops_remaining_targets_and_readback_on_runtime_replacement() {
        use tokio::io::AsyncWriteExt;
        let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", original.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = original.accept().await.unwrap();
            let headers = crate::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_bytes();
            assert!(String::from_utf8_lossy(&headers).starts_with("PUT /providers/proxies/first "));
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), original.accept())
                    .await
                    .is_err(),
                "provider readback or remaining targets reached the retired controller"
            );
        });
        let operations = ProviderOperations::new(client.clone());
        let updating = tokio::spawn(async move {
            operations
                .update_many(vec![
                    ProviderUpdateTarget::new(ProviderKind::Proxy, "first"),
                    ProviderUpdateTarget::new(ProviderKind::Proxy, "second"),
                ])
                .await
        });
        entered_rx.await.unwrap();
        let switching = queue_runtime_switch(client, MihomoEndpoint::new(replacement_url, ""));
        release_tx.send(()).unwrap();
        server.await.unwrap();
        finish_runtime_switch(switching).await;
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), updating)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!receipt.failures().is_empty());
        assert!(receipt.client().verify_current_runtime().is_err());
        assert!(receipt.client().runtime_config().await.is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
                .await
                .is_err()
        );
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
                let request =
                    crate::test_support::read_controller_request_blocking(&mut stream).unwrap();
                let request = request.headers();
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
