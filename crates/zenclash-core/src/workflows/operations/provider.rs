//! Last-known-good state for provider update and health-check operations.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use thiserror::Error;

use crate::{MihomoClient, Provider, ProviderCatalog};

mod action_receipt;
mod actions;
mod session_resources;
pub use action_receipt::ProviderActionReceipt;
pub use session_resources::{CoreResourceAction, ResourceActionReceipt};

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
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "provider_operations.errors.operation",
            &[("error", .0.clone())]
        )
    )]
    Operation(String),
    /// A successful catalog did not contain the requested provider.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "provider_operations.errors.missing_readback",
            &[("name", .0.clone())]
        )
    )]
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
mod tests;
