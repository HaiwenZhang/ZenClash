mod actions;
mod state;
mod view;
pub(crate) mod workflow;

pub(crate) use state::ProfileFormState;

use zenclash_core::{ProfileCatalog, ProfileRecovery, ProfileStore};

pub(super) struct ProfileLibrary {
    pub(super) store: Option<ProfileStore>,
    pub(super) catalog: ProfileCatalog,
    pub(super) recovery: Option<ProfileRecovery>,
    pub(super) pending_finalization: Option<u64>,
    pub(super) forms: ProfileFormState,
    pub(super) generation: u64,
    pub(super) read_task: super::loader::PageReadTask,
}

impl ProfileLibrary {
    pub(super) fn active_profile(&self) -> Option<&zenclash_core::ProfileRecord> {
        self.forms.catalog_view.active_profile(&self.catalog)
    }

    pub(super) fn new(
        store: Option<ProfileStore>,
        catalog: ProfileCatalog,
        mut forms: ProfileFormState,
    ) -> Self {
        forms.catalog_view.prepare(&catalog);
        Self {
            store,
            catalog,
            recovery: None,
            pending_finalization: None,
            forms,
            generation: 0,
            read_task: super::loader::PageReadTask::default(),
        }
    }

    pub(super) fn begin_read(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub(super) fn accept_catalog(&mut self, generation: u64, catalog: ProfileCatalog) -> bool {
        if generation != self.generation {
            return false;
        }
        self.forms.catalog_view.prepare(&catalog);
        self.catalog = catalog;
        true
    }
}
