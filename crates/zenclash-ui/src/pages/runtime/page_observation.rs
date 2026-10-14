//! One owner for ordinary page-read identity, cancellation, cache and publication eligibility.

#[cfg(test)]
mod tests;

use crate::runtime_read_source::RuntimeReadSource;
use std::collections::VecDeque;

use zenclash_core::{CoreObservationIdentity, CoreSession};

use super::{Page, RuntimeConfig};
mod snapshot_data;
pub(super) use snapshot_data::PageSnapshotData;
mod observation_time;
pub(super) use observation_time::now_ms;
mod read;
pub(super) use read::load_page;
use read::load_page_with_core;

struct AcceptedPageSnapshot {
    data: PageSnapshotData,
    source: Option<CoreObservationIdentity>,
    configuration_generation: u64,
}

impl Default for AcceptedPageSnapshot {
    fn default() -> Self {
        Self {
            data: PageSnapshotData::Empty,
            source: None,
            configuration_generation: 0,
        }
    }
}

impl AcceptedPageSnapshot {
    fn replace(
        &mut self,
        data: PageSnapshotData,
        source: Option<CoreObservationIdentity>,
        configuration_generation: u64,
    ) {
        let data = if self.configuration_generation == configuration_generation {
            data.retain_dashboard_successes(&self.data)
        } else {
            data
        };
        *self = Self {
            data,
            source,
            configuration_generation,
        };
    }
}

/// Owns cancellation of the page's read-only request, including when the view is dropped.
#[derive(Debug, Default)]
pub(super) struct PageReadTask(Option<tokio::task::AbortHandle>);

impl PageReadTask {
    pub(super) fn replace<T>(&mut self, task: &tokio::task::JoinHandle<T>) {
        self.cancel();
        self.0 = Some(task.abort_handle());
    }

    pub(super) fn cancel(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl Drop for PageReadTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PageReadIdentity {
    page: Page,
    navigation_generation: u64,
    request_sequence: u64,
}

pub(super) struct ObservedPage {
    runtime: CoreObservationIdentity,
    result: Result<PageSnapshotData, String>,
}

struct CachedPageObservation {
    page: Page,
    runtime: CoreObservationIdentity,
    data: PageSnapshotData,
}

#[derive(Default)]
pub(super) struct PageObservationOwner {
    snapshot: AcceptedPageSnapshot,
    next_request_sequence: u64,
    active: Option<PageReadIdentity>,
    task: PageReadTask,
    cache: VecDeque<CachedPageObservation>,
}

impl PageObservationOwner {
    pub(super) fn new(configuration_generation: u64) -> Self {
        Self {
            snapshot: AcceptedPageSnapshot {
                configuration_generation,
                ..AcceptedPageSnapshot::default()
            },
            ..Self::default()
        }
    }

    pub(super) fn data(&self) -> &PageSnapshotData {
        &self.snapshot.data
    }

    pub(super) fn source(&self) -> &Option<CoreObservationIdentity> {
        &self.snapshot.source
    }

    pub(super) fn configuration_generation(&self) -> u64 {
        self.snapshot.configuration_generation
    }

    /// The caller publishes under the original observation's memory-only guard.
    pub(super) fn adopt(&mut self, data: PageSnapshotData, source: CoreObservationIdentity) {
        let generation = source.configuration_generation();
        self.snapshot.replace(data, Some(source), generation);
    }

    pub(super) fn replace_unobserved(&mut self, data: PageSnapshotData, generation: u64) {
        self.snapshot.replace(data, None, generation);
    }

    pub(super) fn config(&self, current_generation: u64) -> Option<&RuntimeConfig> {
        if self.configuration_generation() != current_generation {
            return None;
        }
        match self.data() {
            PageSnapshotData::Dashboard { config, .. } => config.value(),
            PageSnapshotData::Profile { config, .. }
            | PageSnapshotData::Settings { config, .. } => config.as_ref(),
            PageSnapshotData::Config(config)
            | PageSnapshotData::Core { config, .. }
            | PageSnapshotData::Resources { config, .. }
            | PageSnapshotData::SystemProxy { config, .. }
            | PageSnapshotData::Network { config, .. }
            | PageSnapshotData::Tun { config, .. } => Some(config),
            _ => None,
        }
    }

    pub(super) fn dashboard_catalog_mut(&mut self) -> Option<&mut zenclash_core::ProxyCatalog> {
        match &mut self.snapshot.data {
            PageSnapshotData::Dashboard {
                proxies:
                    zenclash_core::Observation::Fresh { value, .. }
                    | zenclash_core::Observation::Stale { value, .. },
                ..
            } => Some(value),
            _ => None,
        }
    }

    pub(super) fn retain_current(&mut self, page: Page) {
        let data = std::mem::replace(&mut self.snapshot.data, PageSnapshotData::Empty);
        let source = self.snapshot.source.take();
        self.retain(page, source, data);
    }

    pub(super) fn release_data(&mut self) {
        self.snapshot.data = PageSnapshotData::Empty;
    }

    pub(super) fn restore_current(&mut self, page: Page, session: &CoreSession) {
        self.snapshot.data = PageSnapshotData::Empty;
        self.snapshot.source = None;
        if let Some(observed) = self.restore(page, session) {
            observed.publish(|result, runtime| {
                if let Ok(data) = result {
                    self.snapshot = AcceptedPageSnapshot {
                        data,
                        configuration_generation: runtime.configuration_generation(),
                        source: Some(runtime),
                    };
                }
            });
        }
    }

    pub(super) fn reuse_config(
        &mut self,
        config: Option<RuntimeConfig>,
        source: Option<CoreObservationIdentity>,
        current_generation: u64,
    ) {
        if matches!(self.data(), PageSnapshotData::Empty)
            && self.configuration_generation() == current_generation
            && let Some(config) = config
        {
            self.snapshot.data = PageSnapshotData::Config(config);
            self.snapshot.source = source;
        }
        self.snapshot.configuration_generation = current_generation;
    }

    pub(super) fn restore_for_resume(&mut self, page: Page, session: &CoreSession) {
        let generation = session.generation();
        self.restore_current(page, session);
        self.snapshot.configuration_generation = generation;
    }

    #[cfg(test)]
    pub(super) fn replace_fixture_data(&mut self, data: PageSnapshotData) {
        self.snapshot.data = data;
    }

    #[cfg(test)]
    pub(super) fn set_fixture_source(&mut self, source: Option<CoreObservationIdentity>) {
        self.snapshot.source = source;
    }

    #[cfg(test)]
    pub(super) fn set_fixture_generation(&mut self, generation: u64) {
        self.snapshot.configuration_generation = generation;
    }

    pub(super) fn query_revision(&self) -> u64 {
        self.next_request_sequence
    }
    const CACHE_CAPACITY: usize = 4;
    pub(super) fn begin(&mut self, page: Page, navigation_generation: u64) -> PageReadIdentity {
        self.cancel();
        self.next_request_sequence = self.next_request_sequence.wrapping_add(1);
        let identity = PageReadIdentity {
            page,
            navigation_generation,
            request_sequence: self.next_request_sequence,
        };
        self.active = Some(identity);
        identity
    }
    pub(super) fn track<T>(
        &mut self,
        request: PageReadIdentity,
        task: &tokio::task::JoinHandle<T>,
    ) {
        if self.active == Some(request) {
            self.task.replace(task);
        } else {
            task.abort();
        }
    }
    pub(super) fn finish(
        &mut self,
        request: PageReadIdentity,
        page: Page,
        navigation_generation: u64,
    ) -> bool {
        if self.active != Some(request) {
            return false;
        }
        self.active = None;
        self.task.cancel();
        request.page == page && request.navigation_generation == navigation_generation
    }
    pub(super) fn cancel(&mut self) {
        self.active = None;
        self.task.cancel();
    }
    pub(super) fn clear_cache(&mut self) {
        self.cache.clear();
    }
    pub(super) fn retain(
        &mut self,
        page: Page,
        runtime: Option<CoreObservationIdentity>,
        data: PageSnapshotData,
    ) {
        let Some(runtime) = runtime.filter(CoreObservationIdentity::is_current) else {
            return;
        };
        if matches!(data, PageSnapshotData::Empty) {
            return;
        }
        self.cache
            .retain(|entry| entry.page != page && entry.runtime.is_current());
        if self.cache.len() == Self::CACHE_CAPACITY {
            self.cache.pop_front();
        }
        self.cache.push_back(CachedPageObservation {
            page,
            runtime,
            data,
        });
    }
    pub(super) fn restore(&mut self, page: Page, session: &CoreSession) -> Option<ObservedPage> {
        self.cache
            .retain(|entry| entry.runtime.belongs_to(session) && entry.runtime.is_current());
        let index = self.cache.iter().position(|entry| entry.page == page)?;
        let entry = self.cache.remove(index)?;
        Some(ObservedPage {
            runtime: entry.runtime,
            result: Ok(entry.data),
        })
    }
}

impl ObservedPage {
    pub(super) fn belongs_to_observation(&self, source: &CoreObservationIdentity) -> bool {
        self.runtime.is_same_observation(source)
    }

    pub(super) async fn load_from_observation(
        session: CoreSession,
        observation: zenclash_core::CoreObservation,
        page: Page,
    ) -> Result<Self, String> {
        if !observation.identity().belongs_to(&session) {
            return Err(zenclash_core::MihomoError::StaleBinding.to_string());
        }
        observation
            .wait_for_read_eligibility()
            .await
            .map_err(|error| error.to_string())?;
        let result = load_page_with_core(observation.client().clone(), page, Some(session)).await;
        observation
            .wait_for_read_eligibility()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            runtime: observation.identity().clone(),
            result,
        })
    }

    #[cfg(test)]
    pub(super) async fn load_for_runtime(
        session: CoreSession,
        client: super::MihomoClient,
        page: Page,
    ) -> Result<Self, String> {
        let observation = session
            .begin_observation_for_runtime(&client)
            .await
            .map_err(|error| error.to_string())?;
        let runtime = observation.identity().clone();
        let result = load_page_with_core(observation.client().clone(), page, Some(session)).await;
        Ok(Self { runtime, result })
    }

    #[cfg(test)]
    pub(super) async fn load(session: CoreSession, page: Page) -> Result<Self, String> {
        Self::load_with_source(session, page, &RuntimeReadSource::default()).await
    }

    pub(super) async fn load_with_source(
        session: CoreSession,
        page: Page,
        source: &RuntimeReadSource,
    ) -> Result<Self, String> {
        let observation = source.capture(&session).await?;
        let runtime = observation.identity().clone();
        let result = load_page_with_core(observation.client().clone(), page, Some(session)).await;
        Ok(Self { runtime, result })
    }
    pub(super) fn publish(
        self,
        accept: impl FnOnce(Result<PageSnapshotData, String>, CoreObservationIdentity),
    ) -> bool {
        let runtime = self.runtime;
        runtime
            .publish_if_current(|| accept(self.result, runtime.clone()))
            .is_some()
    }
}
