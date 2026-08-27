use std::path::{Path, PathBuf};

use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreSession, EffectiveConfigIntent, MihomoClient,
    ProfileApplication, ProfileApplyOutcome, ProfileChange, ProfileStore, ProfileStoreResult,
    RemoteProfileOptions, YamlOverrideStore,
};

use super::super::{Page, RuntimeData, load_page};

pub(crate) struct ActivationOutcome {
    pub(in crate::pages::runtime) refresh: Result<RuntimeData, String>,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
}

pub(super) struct UpdateOutcome {
    pub(super) refresh: Result<RuntimeData, String>,
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) active: bool,
}

pub(crate) struct BackgroundUpdateOutcome {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) active: bool,
}

#[derive(Clone)]
pub(crate) struct CoreProfileRuntime {
    session: CoreSession,
}

impl CoreProfileRuntime {
    pub(crate) fn new(session: CoreSession) -> Self {
        Self { session }
    }

    pub(crate) fn client(&self) -> &MihomoClient {
        self.session.client()
    }

    pub(crate) fn kind(&self) -> CoreKind {
        self.session.kind()
    }

    pub(crate) async fn reload_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
    ) -> Result<(), String> {
        self.session
            .apply(
                &controlled,
                EffectiveConfigIntent::ActivateProfile {
                    profile: path.to_path_buf(),
                    overrides,
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

pub(super) async fn import_local(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    source: PathBuf,
    refresh_page: Page,
) -> Result<ActivationOutcome, String> {
    let overrides = load_enabled_overrides().await?;
    let rejected = zenclash_i18n::text_with(
        "profiles.errors.rejected",
        &[("core", runtime.kind().display_name().to_owned())],
    );
    apply_new_profile_change(
        store,
        controlled,
        runtime,
        ProfileChange::ImportLocal { source, overrides },
        &rejected,
        refresh_page,
    )
    .await
}

pub(in super::super) async fn add_remote(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    name: String,
    url: String,
    user_agent: String,
    options: RemoteProfileOptions,
) -> Result<ActivationOutcome, String> {
    let overrides = load_enabled_overrides().await?;
    let rejected = zenclash_i18n::text_with(
        "profiles.errors.downloaded_rejected",
        &[("core", runtime.kind().display_name().to_owned())],
    );
    apply_new_profile_change(
        store,
        controlled,
        runtime,
        ProfileChange::AddRemote {
            name,
            url,
            user_agent,
            options,
            overrides,
        },
        &rejected,
        Page::Profiles,
    )
    .await
}

pub(crate) async fn activate_existing(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
) -> Result<ActivationOutcome, String> {
    activate_existing_for_page(store, controlled, runtime, id, Page::Profiles).await
}

pub(in crate::pages::runtime) async fn activate_existing_for_page(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
    refresh_page: Page,
) -> Result<ActivationOutcome, String> {
    let overrides = load_enabled_overrides().await?;
    let application = ProfileApplication::new(store, controlled, runtime.session.clone());
    match application
        .apply(ProfileChange::ActivateExisting { id, overrides })
        .await
    {
        ProfileApplyOutcome::Applied { profile, path, .. } => {
            let refresh = load_page(runtime.client().clone(), refresh_page).await;
            Ok(ActivationOutcome {
                refresh,
                path,
                name: profile.name,
            })
        }
        ProfileApplyOutcome::Stored { .. } => {
            Err(zenclash_i18n::text("profiles.errors.not_applied"))
        }
        ProfileApplyOutcome::Rejected { cause, .. } => Err(cause.to_string()),
        ProfileApplyOutcome::RolledBack { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.rejected_rolled_back",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::RuntimeUnknown { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.runtime_unknown",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::PersistedButRuntimeUnknown {
            cause, rollback, ..
        } => Err(zenclash_i18n::text_with(
            "profiles.errors.rejected_rollback_failed",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
                ("rollback", rollback.to_string()),
            ],
        )),
    }
}

pub(super) async fn update_remote(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
) -> Result<UpdateOutcome, String> {
    let outcome = update_remote_background(store, controlled, runtime.clone(), id).await?;
    let refresh = load_page(runtime.client().clone(), Page::Profiles).await;
    Ok(UpdateOutcome {
        refresh,
        path: outcome.path,
        name: outcome.name,
        active: outcome.active,
    })
}

pub(crate) async fn update_remote_background(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
) -> Result<BackgroundUpdateOutcome, String> {
    let overrides = load_enabled_overrides().await?;
    let application = ProfileApplication::new(store, controlled, runtime.session.clone());
    match application
        .apply(ProfileChange::UpdateRemote { id, overrides })
        .await
    {
        ProfileApplyOutcome::Applied { profile, path, .. } => Ok(BackgroundUpdateOutcome {
            path,
            name: profile.name,
            active: true,
        }),
        ProfileApplyOutcome::Stored { profile, path, .. } => Ok(BackgroundUpdateOutcome {
            path,
            name: profile.name,
            active: false,
        }),
        ProfileApplyOutcome::Rejected { cause, .. } => Err(cause.to_string()),
        ProfileApplyOutcome::RolledBack { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.update_rejected_rolled_back",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::RuntimeUnknown { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.runtime_unknown",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::PersistedButRuntimeUnknown {
            cause, rollback, ..
        } => Err(zenclash_i18n::text_with(
            "profiles.errors.update_rejected_rollback_failed",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
                ("rollback", rollback.to_string()),
            ],
        )),
    }
}

pub(super) async fn delete(store: ProfileStore, id: String) -> Result<(), String> {
    run_store(move || store.delete(&id)).await
}

async fn apply_new_profile_change(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    change: ProfileChange,
    rejection_prefix: &str,
    refresh_page: Page,
) -> Result<ActivationOutcome, String> {
    let application = ProfileApplication::new(store, controlled, runtime.session.clone());
    match application.apply(change).await {
        ProfileApplyOutcome::Applied { profile, path, .. } => {
            let refresh = load_page(runtime.client().clone(), refresh_page).await;
            Ok(ActivationOutcome {
                refresh,
                path,
                name: profile.name,
            })
        }
        ProfileApplyOutcome::Stored { .. } => {
            Err(zenclash_i18n::text("profiles.errors.not_applied"))
        }
        ProfileApplyOutcome::Rejected { cause, .. } => Err(cause.to_string()),
        ProfileApplyOutcome::RolledBack { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.selection_rolled_back",
            &[
                ("prefix", rejection_prefix.to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::RuntimeUnknown { cause, .. } => Err(zenclash_i18n::text_with(
            "profiles.errors.runtime_unknown",
            &[
                ("core", runtime.kind().display_name().to_owned()),
                ("error", cause.to_string()),
            ],
        )),
        ProfileApplyOutcome::PersistedButRuntimeUnknown {
            cause, rollback, ..
        } => Err(zenclash_i18n::text_with(
            "profiles.errors.selection_rollback_failed",
            &[
                ("prefix", rejection_prefix.to_owned()),
                ("error", cause.to_string()),
                ("rollback", rollback.to_string()),
            ],
        )),
    }
}

pub(in super::super) async fn reload_effective(
    controlled: ControlledConfigStore,
    runtime: &CoreProfileRuntime,
    path: &Path,
) -> Result<(), String> {
    let overrides = load_enabled_overrides().await?;
    runtime
        .reload_with_overrides(controlled, path, overrides)
        .await
}

pub(in crate::pages::runtime) async fn load_enabled_overrides() -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.override_read_task",
                &[("error", error.to_string())],
            )
        })?
        .map_err(|error| error.to_string())
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
