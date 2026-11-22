//! Owns persistent override-chain changes, runtime application, and compensation.

use std::{fmt, path::PathBuf};

use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, YamlOverrideCatalog, YamlOverrideStore,
};

use super::ApplicationCommands;

mod changes;
mod compensation;
#[cfg(test)]
mod tests;

/// Coordinates YAML override-chain mutations, runtime reapplication, and
/// disk compensation inside serialized data-application admissions.
#[derive(Clone)]
pub struct OverrideApplication {
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

    /// Imports YAML override files and applies the updated override chain to
    /// the running core when a profile is active.
    ///
    /// # Errors
    ///
    /// Returns [`OverrideApplicationFailure`] if import, runtime application,
    /// or compensation fails.
    pub async fn import_paths(
        &self,
        paths: Vec<PathBuf>,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::Import { paths })
            .await
    }

    /// Atomically replaces the override catalog using compare-and-swap against
    /// `expected`, then applies the updated chain to the running core.
    ///
    /// # Errors
    ///
    /// Returns [`OverrideApplicationFailure`] if the catalog changed
    /// concurrently, runtime application fails, or compensation fails.
    pub async fn replace_catalog(
        &self,
        expected: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::ReplaceCatalog { expected, next })
            .await
    }

    /// Deletes a disabled override entry by ID without triggering a runtime reload.
    ///
    /// # Errors
    ///
    /// Returns [`OverrideApplicationFailure`] if deletion fails or the entry is enabled.
    pub async fn delete_disabled(
        &self,
        id: String,
    ) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::DeleteDisabled { id })
            .await
    }

    /// Reapplies the current on-disk override chain to the active profile.
    ///
    /// # Errors
    ///
    /// Returns [`OverrideApplicationFailure`] if reading the catalog or
    /// reapplying the effective configuration fails.
    pub async fn reapply(&self) -> Result<OverrideApplicationReceipt, OverrideApplicationFailure> {
        self.complete(changes::OverrideChange::Reapply).await
    }
}

/// Persistent facts and optional runtime acceptance from one completed override command.
#[derive(Debug)]
pub struct OverrideApplicationReceipt {
    catalog: YamlOverrideCatalog,
    runtime: Option<CoreApplyOutcome>,
    imported_count: usize,
}

impl OverrideApplicationReceipt {
    /// Returns the runtime apply outcome if an active profile was reloaded.
    #[must_use]
    pub fn runtime_outcome(&self) -> Option<&CoreApplyOutcome> {
        self.runtime.as_ref()
    }

    /// Returns the number of newly imported override files.
    #[must_use]
    pub fn imported_count(&self) -> usize {
        self.imported_count
    }

    /// Returns the number of enabled overrides in the resulting catalog.
    #[must_use]
    pub fn enabled_count(&self) -> usize {
        self.catalog
            .items
            .iter()
            .filter(|record| record.enabled)
            .count()
    }

    /// Consumes the receipt and returns the persisted override catalog.
    #[must_use]
    pub fn into_catalog(self) -> YamlOverrideCatalog {
        self.catalog
    }
}

/// Classification of how an override-chain failure left persistent state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverrideFailureKind {
    /// The request did not reach normal runtime application.
    Rejected,
    /// Its persistent changes were removed or restored.
    Compensated,
    /// Persistence or compensation ended without proving a complete restoration.
    CompensationRequired,
}

/// Failure details and post-failure catalog snapshot from an override command.
#[derive(Debug)]
pub struct OverrideApplicationFailure {
    message: String,
    kind: OverrideFailureKind,
    current_catalog: Option<YamlOverrideCatalog>,
    runtime_unconfirmed: bool,
}

impl OverrideApplicationFailure {
    /// Returns whether persistent changes were rejected, compensated, or need manual attention.
    #[must_use]
    pub fn kind(&self) -> OverrideFailureKind {
        self.kind
    }

    /// Consumes the failure and returns the latest catalog observed after failure, if available.
    #[must_use]
    pub fn into_catalog(self) -> Option<YamlOverrideCatalog> {
        self.current_catalog
    }

    /// Returns whether the live runtime state remained unconfirmed after failure (test helper).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn is_runtime_unconfirmed(&self) -> bool {
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
