use zenclash_core::{YamlOverrideCatalog, YamlOverrideRecord};

use super::{OverrideApplication, OverrideApplicationFailure, OverrideFailureKind};

impl OverrideApplication {
    pub(super) async fn compensate_import(
        &self,
        imported: Vec<YamlOverrideRecord>,
        cause: String,
        runtime_unconfirmed: bool,
    ) -> OverrideApplicationFailure {
        let store = self.store.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            let mut failures = Vec::new();
            for record in imported {
                if let Err(error) = store.discard_imported(&record) {
                    failures.push(error.to_string());
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("；"))
            }
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
        let core = self.application_commands.kind().display_name().to_owned();
        let (message, kind) = match cleanup {
            Ok(()) => (
                zenclash_i18n::text_with(
                    "overrides.errors.import_rejected_removed",
                    &[("core", core), ("error", cause)],
                ),
                OverrideFailureKind::Compensated,
            ),
            Err(error) => (
                zenclash_i18n::text_with(
                    "overrides.errors.import_rejected_cleanup_failed",
                    &[("core", core), ("error", cause), ("cleanup", error)],
                ),
                OverrideFailureKind::CompensationRequired,
            ),
        };
        self.failure(message, kind, runtime_unconfirmed).await
    }

    pub(super) async fn compensate_catalog(
        &self,
        persisted: YamlOverrideCatalog,
        previous: YamlOverrideCatalog,
        cause: String,
        runtime_unconfirmed: bool,
    ) -> OverrideApplicationFailure {
        let store = self.store.clone();
        let rollback =
            tokio::task::spawn_blocking(move || store.replace_catalog(&persisted, &previous))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
        let core = self.application_commands.kind().display_name().to_owned();
        let (message, kind) = match rollback {
            Ok(()) => (
                zenclash_i18n::text_with(
                    "overrides.errors.change_rejected_rolled_back",
                    &[("core", core), ("error", cause)],
                ),
                OverrideFailureKind::Compensated,
            ),
            Err(error) => (
                zenclash_i18n::text_with(
                    "overrides.errors.change_rejected_rollback_failed",
                    &[("core", core), ("error", cause), ("rollback", error)],
                ),
                OverrideFailureKind::CompensationRequired,
            ),
        };
        self.failure(message, kind, runtime_unconfirmed).await
    }
}
