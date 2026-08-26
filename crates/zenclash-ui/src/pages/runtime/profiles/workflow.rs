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
            Err(format!(
                "外部 {} 不支持完整配置热重载；请由 ZenClash 托管该内核后重试",
                self.kind.display_name()
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
    let rejected = format!("{} 拒绝该配置", runtime.kind.display_name());
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
    activate_new_record(
        store,
        controlled,
        runtime.clone(),
        record,
        &format!("下载成功，但 {} 拒绝该配置", runtime.kind.display_name()),
    )
    .await
}

pub(crate) async fn activate_existing(
    store: ProfileStore,
    controlled: ControlledConfigStore,
    runtime: CoreProfileRuntime,
    id: String,
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
            Ok(()) => Err(format!(
                "{} 拒绝该配置，活动选择已恢复：{error}",
                runtime.kind.display_name()
            )),
            Err(rollback) => Err(format!(
                "{} 拒绝该配置：{error}；恢复原活动配置失败：{rollback}",
                runtime.kind.display_name()
            )),
        };
    }
    let refresh = load_page(runtime.client, Page::Profiles).await;
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
        Err(error) => return Err(format!("无法读取当前内核的订阅代理端口：{error}")),
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
        Err("当前内核没有可用的 HTTP/Mixed 订阅代理端口".into())
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
    let activation = match run_store(move || activation_store.activate_reversible(&activation_id))
        .await
    {
        Ok(activation) => activation,
        Err(error) => {
            return Err(
                cleanup_new_record(store, record.id, format!("保存活动配置失败：{error}")).await,
            );
        }
    };
    let path = activation.path().to_path_buf();
    if let Err(error) = reload_effective(controlled, &runtime, &path).await {
        let rollback_store = store.clone();
        let primary = match run_store(move || rollback_store.rollback_activation(activation)).await
        {
            Ok(()) => format!("{rejection_prefix}，活动选择已恢复：{error}"),
            Err(rollback) => {
                return Err(format!(
                    "{rejection_prefix}：{error}；恢复原活动配置失败：{rollback}"
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
            Ok(_) => Err(format!(
                "{} 拒绝订阅更新，已恢复上一版本：{error}",
                runtime.kind.display_name()
            )),
            Err(rollback) => Err(format!(
                "{} 拒绝订阅更新：{error}；恢复上一版本失败：{rollback}",
                runtime.kind.display_name()
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
            .map_err(|error| format!("读取 YAML 覆写任务异常结束：{error}"))?
            .map_err(|error| error.to_string())?;
    runtime
        .reload_with_overrides(controlled, path, overrides)
        .await
}

async fn cleanup_new_record(store: ProfileStore, id: String, primary: String) -> String {
    match run_store(move || store.delete(&id)).await {
        Ok(()) => primary,
        Err(cleanup) => format!("{primary}；清理未启用配置失败：{cleanup}"),
    }
}

async fn run_store<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> ProfileStoreResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("配置仓库后台任务异常结束：{error}"))?
        .map_err(|error| error.to_string())
}
