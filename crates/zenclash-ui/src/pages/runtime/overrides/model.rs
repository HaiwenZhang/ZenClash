//! Override catalog and preview query ownership; application writes remain with their original commands.

use crate::application_commands::{
    OverrideApplicationFailure, OverrideApplicationReceipt, OverrideFailureKind,
};
use std::sync::{Arc, Weak};
use zenclash_core::{ConfigDiffReport, diff_yaml_configs};

const MAX_CONFIG_DIFF_ENTRIES: usize = 200;
use super::MAX_PREVIEW_BYTES;

pub(in crate::pages::runtime) struct ConfigPreview {
    pub(super) source: String,
    pub(super) effective: String,
    pub(super) diff: ConfigDiffReport,
}

impl ConfigPreview {
    pub(in crate::pages::runtime) fn is_truncated(&self) -> bool {
        self.source.len() > MAX_PREVIEW_BYTES || self.effective.len() > MAX_PREVIEW_BYTES
    }
}

pub(in crate::pages::runtime) struct OverridesState {
    owner: Arc<()>,
    store: Option<zenclash_core::YamlOverrideStore>,
    catalog: zenclash_core::YamlOverrideCatalog,
    preview: Option<ConfigPreview>,
    preview_generation: u64,
    preview_loading: bool,
    preview_task: crate::pages::runtime::page_observation::PageReadTask,
    preview_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl OverridesState {
    pub(in crate::pages::runtime) fn new(
        store: Option<zenclash_core::YamlOverrideStore>,
        catalog: zenclash_core::YamlOverrideCatalog,
    ) -> Self {
        Self {
            owner: Arc::new(()),
            store,
            catalog,
            preview: None,
            preview_generation: 0,
            preview_loading: false,
            preview_task: crate::pages::runtime::page_observation::PageReadTask::default(),
            preview_gate: std::sync::Arc::default(),
        }
    }

    pub(in crate::pages::runtime) fn cancel_preview(&mut self) {
        self.preview_task.cancel();
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.preview_loading = false;
    }

    pub(in crate::pages::runtime) fn invalidate_preview(&mut self) {
        self.cancel_preview();
        self.preview = None;
    }

    pub(in crate::pages::runtime) fn enabled_paths(&self) -> Vec<std::path::PathBuf> {
        self.store
            .as_ref()
            .map_or_else(Vec::new, |store| store.enabled_paths(&self.catalog))
    }

    pub(in crate::pages::runtime) fn store(&self) -> Option<&zenclash_core::YamlOverrideStore> {
        self.store.as_ref()
    }
    pub(in crate::pages::runtime) fn catalog(&self) -> &zenclash_core::YamlOverrideCatalog {
        &self.catalog
    }
    pub(in crate::pages::runtime) fn preview(&self) -> Option<&ConfigPreview> {
        self.preview.as_ref()
    }
    pub(super) fn is_preview_loading(&self) -> bool {
        self.preview_loading
    }
    pub(super) fn start_preview(
        &mut self,
        runtime: &tokio::runtime::Handle,
        profile: std::path::PathBuf,
        controlled: zenclash_core::ControlledConfigStore,
    ) -> Option<PreviewRead> {
        if self.preview_loading {
            return None;
        }
        self.invalidate_preview();
        self.preview_loading = true;
        let generation = self.preview_generation;
        let gate = self.preview_gate.clone();
        let overrides = self.enabled_paths();
        let task = runtime.spawn(async move {
            let permit = gate.lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let source = controlled
                    .source_payload(&profile)
                    .map_err(|error| error.to_string())?;
                let effective = controlled
                    .effective_with_overrides(profile, &overrides)
                    .map_err(|error| error.to_string())?;
                let diff = diff_yaml_configs(&source, &effective, MAX_CONFIG_DIFF_ENTRIES)
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(ConfigPreview {
                    source,
                    effective,
                    diff,
                })
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "overrides.errors.preview_task",
                    &[("error", error.to_string())],
                )
            })?
        });

        self.preview_task.replace(&task);
        Some(PreviewRead {
            owner: Arc::downgrade(&self.owner),
            generation,
            task,
        })
    }
    pub(super) fn complete_preview(
        &mut self,
        completion: PreviewCompletion,
    ) -> Option<Result<(), String>> {
        if !Weak::ptr_eq(&completion.owner, &Arc::downgrade(&self.owner))
            || self.preview_generation != completion.generation
        {
            return None;
        }
        self.preview_loading = false;
        Some(
            completion
                .result
                .map(|preview| self.preview = Some(preview)),
        )
    }
    pub(in crate::pages::runtime) fn adopt_bootstrap(
        &mut self,
        store: Option<zenclash_core::YamlOverrideStore>,
        catalog: zenclash_core::YamlOverrideCatalog,
    ) {
        self.store = store;
        self.catalog = catalog;
    }
    pub(in crate::pages::runtime) fn adopt_restored_store(
        &mut self,
        store: zenclash_core::YamlOverrideStore,
    ) {
        self.store = Some(store);
    }
    pub(in crate::pages::runtime) fn adopt_restored_catalog(
        &mut self,
        catalog: zenclash_core::YamlOverrideCatalog,
    ) {
        self.catalog = catalog;
    }
    pub(super) fn adopt_receipt(&mut self, receipt: OverrideApplicationReceipt) {
        self.catalog = receipt.into_catalog();
        self.invalidate_preview();
    }
    pub(super) fn adopt_failure(
        &mut self,
        failure: OverrideApplicationFailure,
    ) -> OverrideFailureFeedback {
        let message = failure.to_string();
        let kind = failure.kind();
        if let Some(catalog) = failure.into_catalog() {
            self.catalog = catalog;
        }
        self.invalidate_preview();
        OverrideFailureFeedback { message, kind }
    }
    #[cfg(test)]
    pub(super) fn replace_fixture_catalog(&mut self, catalog: zenclash_core::YamlOverrideCatalog) {
        self.catalog = catalog;
    }
}

pub(super) struct PreviewRead {
    owner: Weak<()>,
    generation: u64,
    task: tokio::task::JoinHandle<Result<ConfigPreview, String>>,
}
pub(super) struct PreviewCompletion {
    owner: Weak<()>,
    generation: u64,
    result: Result<ConfigPreview, String>,
}
pub(super) struct OverrideFailureFeedback {
    pub(super) message: String,
    pub(super) kind: OverrideFailureKind,
}
impl PreviewRead {
    pub(super) async fn finish(self) -> PreviewCompletion {
        let result = self
            .task
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "overrides.errors.preview_worker",
                    &[("error", error.to_string())],
                )
            })
            .and_then(|result| result);
        PreviewCompletion {
            owner: self.owner,
            generation: self.generation,
            result,
        }
    }
}

#[cfg(test)]
mod tests;
