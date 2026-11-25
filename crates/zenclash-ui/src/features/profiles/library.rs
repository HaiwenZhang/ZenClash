//! Profile catalog read ownership and coherent bootstrap/recovery/form projections.

use super::ProfileFormState;
use std::sync::{Arc, Weak};
use zenclash_core::{ProfileCatalog, ProfileRecovery, ProfileStore};

pub(in crate::features) struct ProfileLibrary {
    owner: Arc<()>,
    store: Option<ProfileStore>,
    catalog: ProfileCatalog,
    recovery: Option<ProfileRecovery>,
    pending_finalization: Option<u64>,
    pub(in crate::features) forms: ProfileFormState,
    generation: u64,
    read_task: crate::runtime_host::observation::PageReadTask,
}

impl ProfileLibrary {
    pub(in crate::features) fn active_profile(&self) -> Option<&zenclash_core::ProfileRecord> {
        self.forms.catalog_view.active_profile(&self.catalog)
    }

    pub(in crate::features) fn new(
        store: Option<ProfileStore>,
        catalog: ProfileCatalog,
        mut forms: ProfileFormState,
    ) -> Self {
        forms.catalog_view.prepare(&catalog);
        Self {
            owner: Arc::new(()),
            store,
            catalog,
            recovery: None,
            pending_finalization: None,
            forms,
            generation: 0,
            read_task: crate::runtime_host::observation::PageReadTask::default(),
        }
    }

    fn begin_read(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn accept_catalog(&mut self, generation: u64, catalog: ProfileCatalog) -> bool {
        if generation != self.generation {
            return false;
        }
        self.forms.catalog_view.prepare(&catalog);
        self.catalog = catalog;
        true
    }

    pub(in crate::features) fn store(&self) -> Option<&ProfileStore> {
        self.store.as_ref()
    }
    pub(in crate::features) fn catalog(&self) -> &ProfileCatalog {
        &self.catalog
    }
    pub(in crate::features) fn recovery(&self) -> &Option<ProfileRecovery> {
        &self.recovery
    }
    pub(in crate::features) fn pending_finalization(&self) -> Option<u64> {
        self.pending_finalization
    }
    pub(in crate::features) fn start_read(
        &mut self,
        runtime: &tokio::runtime::Handle,
    ) -> Option<ProfileCatalogRead> {
        let store = self.store.clone()?;
        let generation = self.begin_read();
        let task = runtime.spawn_blocking(move || {
            let started = std::time::Instant::now();
            let result = store.load().map_err(|error| error.to_string());
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "loaded profile catalog"
            );
            result
        });

        self.read_task.replace(&task);
        Some(ProfileCatalogRead {
            owner: Arc::downgrade(&self.owner),
            generation,
            task,
        })
    }
    pub(in crate::features) fn complete_read(
        &mut self,
        completion: ProfileCatalogCompletion,
    ) -> Option<Result<(), String>> {
        if !Weak::ptr_eq(&completion.owner, &Arc::downgrade(&self.owner))
            || self.generation != completion.generation
        {
            return None;
        }
        Some(completion.result.map(|catalog| {
            self.accept_catalog(completion.generation, catalog);
        }))
    }
    pub(in crate::features) fn adopt_bootstrap(
        &mut self,
        store: Option<ProfileStore>,
        catalog: ProfileCatalog,
    ) {
        self.store = store;
        if self.generation == 0 {
            self.forms.catalog_view.prepare(&catalog);
            self.catalog = catalog;
        }
    }
    pub(in crate::features) fn synchronize_recovery(
        &mut self,
        commands: &crate::ApplicationCommands,
    ) {
        self.recovery = commands.latest_recovery();
        self.pending_finalization = commands.pending_finalization();
    }
    pub(in crate::features) fn adopt_restored_store(&mut self, store: ProfileStore) {
        self.store = Some(store);
    }
    pub(in crate::features) fn adopt_retry_catalog(&mut self, catalog: ProfileCatalog) {
        self.forms.catalog_view.prepare(&catalog);
        self.catalog = catalog;
        self.generation = self.generation.wrapping_add(1);
    }
    pub(in crate::features) fn adopt_restore_catalog(&mut self, catalog: ProfileCatalog) {
        self.generation = self.generation.wrapping_add(1);
        self.forms.catalog_view.prepare(&catalog);
        self.catalog = catalog;
    }
    pub(in crate::features) fn set_catalog_query(&mut self, query: String) {
        self.forms.catalog_view.set_query(query, &self.catalog);
    }
    pub(super) fn set_catalog_filter(&mut self, filter: super::state::ProfileFilter) {
        self.forms.catalog_view.set_filter(filter, &self.catalog);
    }
    #[cfg(test)]
    pub(in crate::features) fn fixture_catalog_mut(&mut self) -> &mut ProfileCatalog {
        &mut self.catalog
    }
    #[cfg(test)]
    pub(in crate::features) fn prepare_fixture_catalog_view(&mut self) {
        self.forms.catalog_view.prepare(&self.catalog);
    }
}

pub(in crate::features) struct ProfileCatalogRead {
    owner: Weak<()>,
    generation: u64,
    task: tokio::task::JoinHandle<Result<ProfileCatalog, String>>,
}
pub(in crate::features) struct ProfileCatalogCompletion {
    owner: Weak<()>,
    generation: u64,
    result: Result<ProfileCatalog, String>,
}
impl ProfileCatalogRead {
    pub(in crate::features) async fn finish(self) -> ProfileCatalogCompletion {
        let result = self
            .task
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
        ProfileCatalogCompletion {
            owner: self.owner,
            generation: self.generation,
            result,
        }
    }
}

#[cfg(test)]
mod tests;
