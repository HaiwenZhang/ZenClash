use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use zenclash_core::{
    ControlledConfigStore, CoreKind, MihomoClient, MihomoProcess, ProfileRecord, ProfileStore,
    ProfileStoreResult, ProfileUpdate, RemoteProfileOptions, RemoteProfileRoute, YamlOverrideStore,
};

use super::super::{load_page, Page, RuntimeData};

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
    kind: CoreKind,
    client: MihomoClient,
    process: Option<Arc<MihomoProcess>>,
}

impl CoreProfileRuntime {
    pub(crate) fn new(
        kind: CoreKind,
        client: MihomoClient,
        process: Option<Arc<MihomoProcess>>,
    ) -> Self {
        Self {
            kind,
            client,
            process,
        }
    }

    pub(crate) fn client(&self) -> &MihomoClient {
        &self.client
    }

    pub(crate) fn kind(&self) -> CoreKind {
        self.kind
    }

    pub(crate) async fn reload_with_overrides(
        &self,
        controlled: ControlledConfigStore,
        path: &Path,
        overrides: Vec<PathBuf>,
    ) -> Result<(), String> {
        if self.kind.capabilities().full_config_reload {
            controlled
                .reload_with_overrides(&self.client, path, overrides)
                .await
                .map_err(|error| error.to_string())
        } else if let Some(process) = self.process.clone() {
            controlled
                .restart_with_overrides(process, path, overrides)
                .await
                .map_err(|error| error.to_string())
        } else {
            Err(zenclash_i18n::text_with(
                "profiles.errors.external_reload",
                &[("core", self.kind.display_name().to_owned())],
            ))
        }
    }
}

pub(super) async fn import_local(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    source: PathBuf,
) -> Result<ActivationOutcome, String> {
    let import_store = store.clone();
    let record = run_store(move || import_store.import_local(source)).await?;
    let rejected = zenclash_i18n::text_with(
        "profiles.errors.rejected",
        &[("core", runtime.kind.display_name().to_owned())],
    );
    activate_new_record(store, controlled, runtime, record, &rejected).await
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
    let proxy_port = subscription_proxy_port(&runtime.client, options.route()).await?;
    let record = store
        .add_remote_with_options(name, url, user_agent, options, proxy_port)
        .await
        .map_err(|error| error.to_string())?;
    let rejected = zenclash_i18n::text_with(
        "profiles.errors.downloaded_rejected",
        &[("core", runtime.kind.display_name().to_owned())],
    );
    activate_new_record(store, controlled, runtime.clone(), record, &rejected).await
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
    let load_store = store.clone();
    let lookup_id = id.clone();
    let record = run_store(move || {
        let catalog = load_store.load()?;
        catalog
            .profiles
            .into_iter()
            .find(|profile| profile.id == lookup_id)
            .ok_or_else(|| zenclash_core::ProfileStoreError::NotFound(lookup_id))
    })
    .await?;
    let activation_store = store.clone();
    let activation_id = id.clone();
    let activation =
        run_store(move || activation_store.activate_reversible(&activation_id)).await?;
    let path = activation.path().to_path_buf();
    if let Err(error) = reload_effective(controlled, &runtime, &path).await {
        let rollback_store = store.clone();
        return match run_store(move || rollback_store.rollback_activation(activation)).await {
            Ok(()) => Err(zenclash_i18n::text_with(
                "profiles.errors.rejected_rolled_back",
                &[
                    ("core", runtime.kind.display_name().to_owned()),
                    ("error", error.clone()),
                ],
            )),
            Err(rollback) => Err(zenclash_i18n::text_with(
                "profiles.errors.rejected_rollback_failed",
                &[
                    ("core", runtime.kind.display_name().to_owned()),
                    ("error", error),
                    ("rollback", rollback),
                ],
            )),
        };
    }
    let refresh = load_page(runtime.client, refresh_page).await;
    Ok(ActivationOutcome {
        refresh,
        path,
        name: record.name,
    })
}

pub(super) async fn update_remote(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
) -> Result<UpdateOutcome, String> {
    let outcome = update_remote_background(store, controlled, runtime.clone(), id).await?;
    let refresh = load_page(runtime.client, Page::Profiles).await;
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
    let route = store
        .remote_route(&id)
        .await
        .map_err(|error| error.to_string())?;
    let proxy_port = subscription_proxy_port(&runtime.client, route).await?;
    let update = store
        .update_remote_with_proxy(&id, proxy_port)
        .await
        .map_err(|error| error.to_string())?;
    let record = update.record.clone();
    let path = store.profile_path(&record);
    let load_store = store.clone();
    let active_id = id.clone();
    let is_active = run_store(move || load_store.load())
        .await?
        .active
        .as_deref()
        == Some(active_id.as_str());
    if is_active {
        reload_updated_profile(&store, controlled, &runtime, &path, update).await?;
    }
    Ok(BackgroundUpdateOutcome {
        path,
        name: record.name,
        active: is_active,
    })
}

async fn subscription_proxy_port(
    client: &MihomoClient,
    route: RemoteProfileRoute,
) -> Result<Option<u16>, String> {
    if route == RemoteProfileRoute::Direct {
        return Ok(None);
    }
    let config = match client.runtime_config().await {
        Ok(config) => config,
        Err(_) if route == RemoteProfileRoute::DirectWithMihomoFallback => return Ok(None),
        Err(error) => {
            return Err(zenclash_i18n::text_with(
                "profiles.errors.proxy_port_read",
                &[("error", error.to_string())],
            ))
        }
    };
    let port = if config.mixed_port != 0 {
        config.mixed_port
    } else {
        config.port
    };
    if port != 0 {
        Ok(Some(port))
    } else if route == RemoteProfileRoute::DirectWithMihomoFallback {
        Ok(None)
    } else {
        Err(zenclash_i18n::text("profiles.errors.proxy_port_missing"))
    }
}

pub(super) async fn delete(store: ProfileStore, id: String) -> Result<(), String> {
    run_store(move || store.delete(&id)).await
}

async fn activate_new_record(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    record: ProfileRecord,
    rejection_prefix: &str,
) -> Result<ActivationOutcome, String> {
    let activation_store = store.clone();
    let activation_id = record.id.clone();
    let activation =
        match run_store(move || activation_store.activate_reversible(&activation_id)).await {
            Ok(activation) => activation,
            Err(error) => {
                let primary =
                    zenclash_i18n::text_with("profiles.errors.active_save", &[("error", error)]);
                return Err(cleanup_new_record(store, record.id, primary).await);
            }
        };
    let path = activation.path().to_path_buf();
    if let Err(error) = reload_effective(controlled, &runtime, &path).await {
        let rollback_store = store.clone();
        let primary = match run_store(move || rollback_store.rollback_activation(activation)).await
        {
            Ok(()) => zenclash_i18n::text_with(
                "profiles.errors.selection_rolled_back",
                &[
                    ("prefix", rejection_prefix.to_owned()),
                    ("error", error.clone()),
                ],
            ),
            Err(rollback) => {
                return Err(zenclash_i18n::text_with(
                    "profiles.errors.selection_rollback_failed",
                    &[
                        ("prefix", rejection_prefix.to_owned()),
                        ("error", error),
                        ("rollback", rollback),
                    ],
                ));
            }
        };
        return Err(cleanup_new_record(store, record.id, primary).await);
    }
    let refresh = load_page(runtime.client, Page::Profiles).await;
    Ok(ActivationOutcome {
        refresh,
        path,
        name: record.name,
    })
}

async fn reload_updated_profile(
    store: &ProfileStore,
    controlled: ControlledConfigStore,
    runtime: &CoreProfileRuntime,
    path: &Path,
    update: ProfileUpdate,
) -> Result<(), String> {
    if let Err(error) = reload_effective(controlled, runtime, path).await {
        let rollback_store = store.clone();
        return match run_store(move || rollback_store.rollback_update(update)).await {
            Ok(_) => Err(zenclash_i18n::text_with(
                "profiles.errors.update_rejected_rolled_back",
                &[
                    ("core", runtime.kind.display_name().to_owned()),
                    ("error", error.clone()),
                ],
            )),
            Err(rollback) => Err(zenclash_i18n::text_with(
                "profiles.errors.update_rejected_rollback_failed",
                &[
                    ("core", runtime.kind.display_name().to_owned()),
                    ("error", error),
                    ("rollback", rollback),
                ],
            )),
        };
    }
    Ok(())
}

pub(in super::super) async fn reload_effective(
    controlled: ControlledConfigStore,
    runtime: &CoreProfileRuntime,
    path: &Path,
) -> Result<(), String> {
    let overrides =
        tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "profiles.errors.override_read_task",
                    &[("error", error.to_string())],
                )
            })?
            .map_err(|error| error.to_string())?;
    runtime
        .reload_with_overrides(controlled, path, overrides)
        .await
}

async fn cleanup_new_record(store: ProfileStore, id: String, primary: String) -> String {
    match run_store(move || store.delete(&id)).await {
        Ok(()) => primary,
        Err(cleanup) => zenclash_i18n::text_with(
            "profiles.errors.cleanup_disabled",
            &[("primary", primary), ("cleanup", cleanup)],
        ),
    }
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
