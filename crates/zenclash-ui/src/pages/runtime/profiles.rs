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
    pub(super) forms: ProfileFormState,
    pub(super) generation: u64,
    pub(super) read_task: super::loader::PageReadTask,
}

impl ProfileLibrary {
    pub(super) fn new(
        store: Option<ProfileStore>,
        catalog: ProfileCatalog,
        forms: ProfileFormState,
    ) -> Self {
        Self {
            store,
            catalog,
            recovery: None,
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
        self.catalog = catalog;
        true
    }
}
