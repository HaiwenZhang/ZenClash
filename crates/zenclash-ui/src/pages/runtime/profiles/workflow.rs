use std::path::{Path, PathBuf};

use zenclash_core::{MihomoClient, ProfileRecord, ProfileStore, ProfileStoreResult, ProfileUpdate};

use super::super::{load_page, Page, RuntimeData};

pub(super) struct ActivationOutcome {
    pub(super) refresh: Result<RuntimeData, String>,
    pub(super) path: PathBuf,
    pub(super) name: String,
}

pub(super) struct UpdateOutcome {
    pub(super) refresh: Result<RuntimeData, String>,
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) active: bool,
}

pub(super) async fn import_local(
    store: ProfileStore,
    client: MihomoClient,
    source: PathBuf,
) -> Result<ActivationOutcome, String> {
    let import_store = store.clone();
    let record = run_store(move || import_store.import_local(source)).await?;
    activate_new_record(store, client, record, "Mihomo 拒绝该配置").await
}

pub(super) async fn add_remote(
    store: ProfileStore,
    client: MihomoClient,
    name: String,
    url: String,
    user_agent: String,
) -> Result<ActivationOutcome, String> {
    let record = store
        .add_remote(name, url, user_agent)
        .await
        .map_err(|error| error.to_string())?;
    activate_new_record(store, client, record, "下载成功，但 Mihomo 拒绝该配置").await
}

pub(super) async fn activate_existing(
    store: ProfileStore,
    client: MihomoClient,
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
    if let Err(error) = client.reload_config(&path, true).await {
        let rollback_store = store.clone();
        return match run_store(move || rollback_store.rollback_activation(activation)).await {
            Ok(()) => Err(format!("Mihomo 拒绝该配置，活动选择已恢复：{error}")),
            Err(rollback) => Err(format!(
                "Mihomo 拒绝该配置：{error}；恢复原活动配置失败：{rollback}"
            )),
        };
    }
    let refresh = load_page(client, Page::Profiles).await;
    Ok(ActivationOutcome {
        refresh,
        path,
        name: record.name,
    })
}

pub(super) async fn update_remote(
    store: ProfileStore,
    client: MihomoClient,
    id: String,
) -> Result<UpdateOutcome, String> {
    let update = store
        .update_remote(&id)
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
        reload_updated_profile(&store, &client, &path, update).await?;
    }
    let refresh = load_page(client, Page::Profiles).await;
    Ok(UpdateOutcome {
        refresh,
        path,
        name: record.name,
        active: is_active,
    })
}

pub(super) async fn delete(store: ProfileStore, id: String) -> Result<(), String> {
    run_store(move || store.delete(&id)).await
}

async fn activate_new_record(
    store: ProfileStore,
    client: MihomoClient,
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
    if let Err(error) = client.reload_config(&path, true).await {
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
    let refresh = load_page(client, Page::Profiles).await;
    Ok(ActivationOutcome {
        refresh,
        path,
        name: record.name,
    })
}

async fn reload_updated_profile(
    store: &ProfileStore,
    client: &MihomoClient,
    path: &Path,
    update: ProfileUpdate,
) -> Result<(), String> {
    if let Err(error) = client.reload_config(path, true).await {
        let rollback_store = store.clone();
        return match run_store(move || rollback_store.rollback_update(update)).await {
            Ok(_) => Err(format!("Mihomo 拒绝订阅更新，已恢复上一版本：{error}")),
            Err(rollback) => Err(format!(
                "Mihomo 拒绝订阅更新：{error}；恢复上一版本失败：{rollback}"
            )),
        };
    }
    Ok(())
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
