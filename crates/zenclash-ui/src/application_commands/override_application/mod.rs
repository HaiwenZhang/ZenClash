//! Owns persistent override-chain changes, runtime application and compensation.

use std::{fmt, path::PathBuf};

use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, YamlOverrideCatalog, YamlOverrideStore,
};

use super::ApplicationCommands;

mod changes;
mod compensation;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct OverrideApplication {
    application_commands: ApplicationCommands,
    store: YamlOverrideStore,
    controlled_config: ControlledConfigStore,
}

impl OverrideApplication {
    pub(super) fn new(
        application_commands: ApplicationCommands,
        store: YamlOverrideStore,
        controlled_config: ControlledConfigStore,
    ) -> Self {
        Self {
            application_commands,
            store,
            controlled_config,
        }
    }
    pub(crate) async fn import_paths(
        &self,
        paths: Vec<PathBuf>,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::Import { paths })
            .await
    }
    pub(crate) async fn replace_catalog(
        &self,
        expected: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::ReplaceCatalog { expected, next })
            .await
    }
    pub(crate) async fn delete_disabled(
        &self,
        id: String,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::DeleteDisabled { id })
            .await
    }
    pub(crate) async fn reapply(
        &self,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::Reapply).await
    }
}

#[derive(Debug)]
/// Persistent facts and optional runtime acceptance from one completed command.
pub(crate) struct OverrideApplicationReceipt {
    catalog: YamlOverrideCatalog,
    runtime: Option<CoreApplyOutcome>,
    imported_count: usize,
}

impl OverrideApplicationReceipt {
    pub(crate) fn runtime_outcome(&self) -> Option<&CoreApplyOutcome> {
        self.runtime.as_ref()
    }
    pub(crate) fn imported_count(&self) -> usize {
        self.imported_count
    }
    pub(crate) fn enabled_count(&self) -> usize {
        self.catalog
            .items
            .iter()
            .filter(|record| record.enabled)
            .count()
    }
    pub(crate) fn into_catalog(self) -> YamlOverrideCatalog {
        self.catalog
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OverrideFailureKind {
    /// The request did not reach normal runtime application.
    Rejected,
    /// Its persistent changes were removed or restored.
    Compensated,
    /// Persistence or compensation ended without proving a complete restoration.
    CompensationRequired,
}

#[derive(Debug)]
pub(crate) struct OverrideApplicationFailure {
    message: String,
    kind: OverrideFailureKind,
    current_catalog: Option<YamlOverrideCatalog>,
    runtime_unconfirmed: bool,
}

impl OverrideApplicationFailure {
    pub(crate) fn kind(&self) -> OverrideFailureKind {
        self.kind
    }
    pub(crate) fn into_catalog(self) -> Option<YamlOverrideCatalog> {
        self.current_catalog
    }
    #[cfg(test)]
    pub(crate) fn is_runtime_unconfirmed(&self) -> bool {
        self.runtime_unconfirmed
    }
}

impl fmt::Display for OverrideApplicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.runtime_unconfirmed {
            return formatter.write_str(&zenclash_i18n::text_with(
                "overrides.errors.runtime_unconfirmed",
                &[("error", self.message.clone())],
            ));
        }
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OverrideApplicationFailure {}
