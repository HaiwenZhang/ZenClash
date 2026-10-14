//! Page presentation adapters over the shared application commands.

use std::path::PathBuf;

use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, ProfileStore, ProfileStoreResult, RemoteProfileOptions,
};

use super::super::{Page, PageSnapshotData, load_page};
use crate::ApplicationCommands;
use crate::application_commands::ProfileReceipt;

pub(crate) struct ActivationOutcome {
    pub(in crate::pages::runtime) refresh: Result<PageSnapshotData, String>,
    pub(crate) name: String,
    pub(in crate::pages::runtime) receipt: ProfileReceipt,
}

pub(super) struct UpdateOutcome {
    pub(super) refresh: Result<PageSnapshotData, String>,
    pub(super) receipt: ProfileReceipt,
    pub(super) refresh_version: u64,
}

async fn activation_presentation(
    application_commands: &ApplicationCommands,
    receipt: ProfileReceipt,
    page: Page,
) -> Result<ActivationOutcome, String> {
    receipt
        .runtime_version()
        .ok_or_else(|| zenclash_i18n::text("profiles.errors.not_applied"))?;
    let refresh = load_page(application_commands.client().clone(), page).await;
    Ok(ActivationOutcome {
        refresh,
        name: receipt.name().to_owned(),
        receipt,
    })
}

pub(super) async fn import_local(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    application_commands: ApplicationCommands,
    source: PathBuf,
    refresh_page: Page,
) -> Result<ActivationOutcome, String> {
    let receipt = application_commands
        .import_local(store, controlled, source)
        .await
        .map_err(|error| error.to_string())?;
    activation_presentation(&application_commands, receipt, refresh_page).await
}

pub(in super::super) async fn add_remote(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    application_commands: ApplicationCommands,
    name: String,
    url: String,
    user_agent: String,
    options: RemoteProfileOptions,
) -> Result<ActivationOutcome, String> {
    let receipt = application_commands
        .add_remote(store, controlled, name, url, user_agent, options)
        .await
        .map_err(|error| error.to_string())?;
    activation_presentation(&application_commands, receipt, Page::Profiles).await
}

pub(in crate::pages::runtime) async fn activate_existing_for_page(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    application_commands: ApplicationCommands,
    id: String,
    refresh_page: Page,
) -> Result<ActivationOutcome, String> {
    let receipt = application_commands
        .activate(store, controlled, id)
        .await
        .map_err(|error| error.to_string())?;
    activation_presentation(&application_commands, receipt, refresh_page).await
}

pub(super) async fn update_remote(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    application_commands: ApplicationCommands,
    id: String,
) -> Result<UpdateOutcome, String> {
    let receipt = application_commands
        .update_remote(store, controlled, id)
        .await
        .map_err(|error| error.to_string())?;
    let refresh_version = application_commands.session().generation();
    let refresh = load_page(application_commands.client().clone(), Page::Profiles).await;
    Ok(UpdateOutcome {
        refresh,
        receipt,
        refresh_version,
    })
}

pub(super) async fn delete(store: ProfileStore, id: String) -> Result<(), String> {
    run_store(move || store.delete(&id)).await
}

pub(in super::super) async fn reload_effective(
    controlled: ControlledConfigStore,
    application_commands: &ApplicationCommands,
) -> Result<CoreApplyOutcome, String> {
    let overrides = application_commands.enabled_overrides().await?;
    application_commands
        .reapply_with_overrides(controlled, overrides)
        .await
        .and_then(|outcome| {
            outcome.ok_or_else(|| zenclash_i18n::text("profiles.errors.not_applied"))
        })
}

async fn run_store<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> ProfileStoreResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.repository_task",
                &[("error", error.to_string())],
            )
        })?
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{CoreKind, CoreSession, MihomoClient, YamlOverrideStore};

    #[tokio::test]
    async fn saved_overrides_remain_stored_when_no_profile_has_been_applied() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-unapplied-overrides-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("override.yaml");
        std::fs::write(&source, "allow-lan: true\n").unwrap();
        let overrides = YamlOverrideStore::new(root.join("yaml-overrides")).unwrap();
        overrides.import_paths([source]).unwrap();
        let catalog = overrides.load().unwrap();
        let paths = overrides.enabled_paths(&catalog);
        let client =
            MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        let commands = ApplicationCommands::new(session.clone(), Some(overrides.clone()));

        assert_eq!(
            commands
                .reapply_with_overrides(controlled.clone(), paths.clone())
                .await
                .unwrap(),
            None
        );
        assert_eq!(overrides.load().unwrap(), catalog);
        assert_eq!(
            std::fs::read_to_string(&paths[0]).unwrap(),
            "allow-lan: true\n"
        );
        assert!(!controlled.runtime_path().exists());
        assert_eq!(session.generation(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
