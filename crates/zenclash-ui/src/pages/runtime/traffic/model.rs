//! Historical query ownership; navigation and destructive-command admission stay with the host.

use std::{
    path::Path,
    sync::{Arc, Weak},
};

use zenclash_core::{TrafficDimension, TrafficHistorySnapshot, TrafficHistoryStore};

use super::{TrafficHistoryQueryState, TrafficHistoryReadIdentity, TrafficRange, unix_millis};

pub(in crate::pages::runtime) struct TrafficHistory {
    owner: Arc<()>,
    runtime: tokio::runtime::Handle,
    store: Option<TrafficHistoryStore>,
    pub(super) state: TrafficHistoryQueryState,
}

pub(in crate::pages::runtime) struct TrafficHistoryRead {
    owner: Weak<()>,
    identity: TrafficHistoryReadIdentity,
    task: tokio::task::JoinHandle<Result<TrafficHistorySnapshot, String>>,
}

pub(in crate::pages::runtime) struct TrafficHistoryReadCompletion {
    owner: Weak<()>,
    identity: TrafficHistoryReadIdentity,
    result: Result<TrafficHistorySnapshot, String>,
}

pub(in crate::pages::runtime) struct TrafficHistoryClearRequest {
    runtime: tokio::runtime::Handle,
    store: TrafficHistoryStore,
}

impl TrafficHistory {
    pub(in crate::pages::runtime) fn new(
        runtime: tokio::runtime::Handle,
        store: Option<TrafficHistoryStore>,
    ) -> Self {
        Self {
            owner: Arc::new(()),
            runtime,
            store,
            state: TrafficHistoryQueryState::default(),
        }
    }

    pub(in crate::pages::runtime) fn store(&self) -> Option<&TrafficHistoryStore> {
        self.store.as_ref()
    }

    pub(super) fn source(&self) -> Option<&Path> {
        self.store().map(TrafficHistoryStore::path)
    }

    pub(in crate::pages::runtime) fn is_loading(&self) -> bool {
        self.state.loading
    }

    pub(in crate::pages::runtime) fn is_clear_confirmation_open(&self) -> bool {
        self.state.clear_confirmation
    }

    #[cfg(test)]
    pub(in crate::pages::runtime) fn completed_query(
        &self,
    ) -> Option<&super::query_presentation::TrafficHistoryPresentation> {
        self.state.completed_query()
    }

    pub(super) fn presentation(
        &self,
    ) -> Option<&super::query_presentation::TrafficHistoryPresentation> {
        self.state.presentation_for_source(self.source())
    }

    pub(in crate::pages::runtime) fn start_read(&mut self) -> Result<TrafficHistoryRead, String> {
        let store = self
            .store
            .clone()
            .ok_or_else(|| zenclash_i18n::text("traffic.errors.database_open"))?;
        let identity = self
            .state
            .begin_query(store.path().to_path_buf(), unix_millis());
        let plan = identity.plan();
        let query_gate = self.state.query_gate.clone();
        let task = self.runtime.spawn(async move {
            let permit = query_gate.lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                store
                    .query(plan.query(), plan.selection())
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "traffic.errors.query_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        self.state.task.replace(&task);
        Ok(TrafficHistoryRead {
            owner: Arc::downgrade(&self.owner),
            identity,
            task,
        })
    }

    pub(in crate::pages::runtime) fn complete_read(
        &mut self,
        completion: TrafficHistoryReadCompletion,
    ) -> bool {
        if !Weak::ptr_eq(&completion.owner, &Arc::downgrade(&self.owner)) {
            return false;
        }
        let source = self.store.as_ref().map(TrafficHistoryStore::path);
        self.state.complete_query(
            &completion.identity,
            source,
            completion.result,
            unix_millis(),
        )
    }

    pub(super) fn change_range(&mut self, range: TrafficRange) -> bool {
        self.state.change_range(range)
    }

    pub(super) fn change_dimension(&mut self, dimension: TrafficDimension) -> bool {
        self.state.change_dimension(dimension)
    }

    pub(super) fn select_parent(&mut self, label: String) -> bool {
        self.state
            .select_parent(label, self.store.as_ref().map(TrafficHistoryStore::path))
    }

    pub(super) fn select_detail(&mut self, label: String) -> bool {
        self.state
            .select_detail(label, self.store.as_ref().map(TrafficHistoryStore::path))
    }

    pub(super) fn request_clear_confirmation(&mut self) {
        self.state.clear_confirmation = true;
    }

    pub(super) fn cancel_clear_confirmation(&mut self) {
        self.state.clear_confirmation = false;
    }

    /// Captures the original store without starting a write or invalidating reads.
    pub(super) fn prepare_clear(&self) -> Result<TrafficHistoryClearRequest, String> {
        let store = self
            .store
            .clone()
            .ok_or_else(|| zenclash_i18n::text("traffic.errors.database_unavailable"))?;
        Ok(TrafficHistoryClearRequest {
            runtime: self.runtime.clone(),
            store,
        })
    }

    /// Called only after the host admits the original TrafficHistory mutation.
    pub(super) fn invalidate_for_clear(&mut self) {
        self.state.cancel_query();
        self.state.query_revision = self.state.query_revision.wrapping_add(1);
    }

    pub(super) fn complete_clear(&mut self, result: &Result<(), String>) {
        self.state.clear_confirmation = false;
        if result.is_ok() {
            self.state.release_results();
        }
    }

    pub(in crate::pages::runtime) fn cancel_query(&mut self) {
        self.state.cancel_query();
    }

    pub(in crate::pages::runtime) fn release_results(&mut self) {
        self.state.release_results();
    }

    #[cfg(test)]
    pub(in crate::pages::runtime) fn replace_store_for_test(&mut self, store: TrafficHistoryStore) {
        self.store = Some(store);
    }

    #[cfg(test)]
    pub(super) fn query_gate_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.state.query_gate.clone()
    }
}

impl TrafficHistoryRead {
    pub(in crate::pages::runtime) async fn finish(self) -> TrafficHistoryReadCompletion {
        let result = self
            .task
            .await
            .map_err(|error| {
                zenclash_i18n::text_with("traffic.errors.task", &[("error", error.to_string())])
            })
            .and_then(|result| result);
        TrafficHistoryReadCompletion {
            owner: self.owner,
            identity: self.identity,
            result,
        }
    }
}

impl TrafficHistoryClearRequest {
    /// Starts owned completion after host admission; it is never tracked as a cancellable read.
    pub(super) fn start(self) -> tokio::task::JoinHandle<Result<(), String>> {
        self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                self.store.clear().map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "traffic.errors.clear_task",
                    &[("error", error.to_string())],
                )
            })?
        })
    }
}

#[cfg(test)]
mod tests;
