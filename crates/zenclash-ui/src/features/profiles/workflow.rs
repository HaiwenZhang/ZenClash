//! Page presentation adapters over the shared application commands.

use std::path::{Path, PathBuf};

use zenclash_core::{
    ControlledConfigStore, CoreApplyOutcome, CoreLifecyclePhase, ProfileStore,
    RemoteProfileOptions, WorkspaceResources, bundled_default_profile,
};

use crate::ApplicationCommands;
use crate::application_commands::ProfileReceipt;
use crate::foundation::actions::Page;
use crate::runtime_host::observation::PageSnapshotData;
use crate::runtime_host::observation::load_page;

pub(crate) struct ActivationOutcome {
    pub(in crate::features) refresh: Result<PageSnapshotData, String>,
    pub(crate) name: String,
    pub(in crate::features) receipt: ProfileReceipt,
}

pub(super) struct UpdateOutcome {
    pub(super) refresh: Result<PageSnapshotData, String>,
    pub(super) receipt: ProfileReceipt,
    pub(super) refresh_version: u64,
}

pub(super) struct DeleteOutcome {
    pub(super) was_active: bool,
    pub(super) refresh: Option<PageSnapshotData>,
    pub(super) refresh_version: u64,
}

fn fallback_default_profile() -> Option<PathBuf> {
    bundled_default_profile().or_else(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
        let fallback = WorkspaceResources::new(root).default_profile();
        fallback.is_file().then_some(fallback)
    })
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

pub(in crate::features) async fn activate_existing_for_page(
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

pub(super) async fn delete(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    application_commands: ApplicationCommands,
    id: String,
) -> Result<DeleteOutcome, String> {
    let outcome = application_commands
        .delete_profile(store, controlled, id, fallback_default_profile())
        .await?;
    let refresh_version = application_commands.session().generation();
    let running = !application_commands.session().is_offline_recovery()
        && application_commands.session().lifecycle_snapshot().phase != CoreLifecyclePhase::Stopped;
    let refresh = if outcome.was_active && running {
        load_page(application_commands.client().clone(), Page::Profiles)
            .await
            .ok()
    } else {
        None
    };
    Ok(DeleteOutcome {
        was_active: outcome.was_active,
        refresh,
        refresh_version,
    })
}

pub(in super::super) async fn reload_effective(
    controlled: ControlledConfigStore,
    application_commands: &ApplicationCommands,
) -> Result<CoreApplyOutcome, String> {
    application_commands
        .reapply_current(controlled)
        .await
        .and_then(|outcome| {
            outcome.ok_or_else(|| zenclash_i18n::text("profiles.errors.not_applied"))
        })
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
