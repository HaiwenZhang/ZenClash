use super::{
    atomic_write, download_profile, normalized_profile_name, normalized_remote_url,
    normalized_user_agent, read_profile_bytes, validate_clash_yaml, ProfileCatalog, ProfileRecord,
    ProfileSource, ProfileStore, ProfileStoreError, ProfileStoreResult, ProfileUpdate,
    RemoteProfileOptions, RemoteProfileRoute, SubscriptionMetadata,
};

impl ProfileStore {
    /// Returns the persisted download route for a remote profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is missing, local, or the catalog
    /// cannot be read.
    pub async fn remote_route(&self, id: &str) -> ProfileStoreResult<RemoteProfileRoute> {
        let store = self.clone();
        let lookup_id = id.to_owned();
        let record = run_store_task(move || {
            let _transaction = store.transaction.lock();
            let catalog = store.load_unlocked()?;
            remote_profile(&catalog, &lookup_id)
        })
        .await?;
        let ProfileSource::Remote { options, .. } = record.source else {
            return Err(ProfileStoreError::NotFound(format!("{id} 不是在线订阅")));
        };
        Ok(options.route())
    }

    /// Downloads, validates, and stores a remote subscription.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty name, invalid subscription URL or
    /// payload, network failure, or persistence failure.
    pub async fn add_remote(
        &self,
        name: impl Into<String>,
        url: impl Into<String>,
        user_agent: impl Into<String>,
    ) -> ProfileStoreResult<ProfileRecord> {
        self.add_remote_with_options(name, url, user_agent, RemoteProfileOptions::default(), None)
            .await
    }

    /// Downloads and stores a remote subscription with authorization and an
    /// optional explicit Mihomo proxy route.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid request options, missing proxy ports,
    /// network/YAML failures, or persistence failures.
    pub async fn add_remote_with_options(
        &self,
        name: impl Into<String>,
        url: impl Into<String>,
        user_agent: impl Into<String>,
        options: RemoteProfileOptions,
        mihomo_proxy_port: Option<u16>,
    ) -> ProfileStoreResult<ProfileRecord> {
        let name = normalized_profile_name(&name.into())?;
        let url = normalized_remote_url(&url.into())?;
        let user_agent = user_agent.into();
        let user_agent = normalized_user_agent(&user_agent)?;
        let downloaded = download_profile(&url, &user_agent, &options, mihomo_proxy_port).await?;
        let store = self.clone();
        run_store_task(move || {
            validate_clash_yaml(&downloaded.payload)?;
            store.store_profile_with_subscription(
                name,
                ProfileSource::Remote {
                    url,
                    user_agent,
                    options,
                },
                &downloaded.payload,
                downloaded.metadata,
            )
        })
        .await
    }

    /// Refreshes a remote profile and returns a reversible update token.
    ///
    /// The catalog is reloaded after the network request, so concurrent store
    /// operations cannot be overwritten by a stale pre-download snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is missing or local, the download or
    /// validation fails, or the file/index transaction cannot be persisted.
    pub async fn update_remote(&self, id: &str) -> ProfileStoreResult<ProfileUpdate> {
        self.update_remote_with_proxy(id, None).await
    }

    /// Refreshes a remote subscription, using `mihomo_proxy_port` when its
    /// persisted policy requires a Mihomo route.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile/request is invalid, the required
    /// proxy is unavailable, or the reversible update cannot be persisted.
    pub async fn update_remote_with_proxy(
        &self,
        id: &str,
        mihomo_proxy_port: Option<u16>,
    ) -> ProfileStoreResult<ProfileUpdate> {
        let source_store = self.clone();
        let source_id = id.to_owned();
        let expected_record = run_store_task(move || {
            let _transaction = source_store.transaction.lock();
            let catalog = source_store.load_unlocked()?;
            remote_profile(&catalog, &source_id)
        })
        .await?;
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &expected_record.source
        else {
            return Err(ProfileStoreError::NotFound(format!(
                "{} 不是在线订阅",
                expected_record.id
            )));
        };
        let (url, user_agent, options) = (url.clone(), user_agent.clone(), options.clone());

        let downloaded = download_profile(&url, &user_agent, &options, mihomo_proxy_port).await?;
        let update_store = self.clone();
        let update_id = id.to_owned();
        run_store_task(move || {
            validate_clash_yaml(&downloaded.payload)?;
            update_store.persist_remote_update(
                &update_id,
                &expected_record,
                downloaded.payload.into_bytes(),
                downloaded.metadata,
            )
        })
        .await
    }

    pub(super) fn persist_remote_update(
        &self,
        id: &str,
        expected_record: &ProfileRecord,
        applied_payload: Vec<u8>,
        subscription: SubscriptionMetadata,
    ) -> ProfileStoreResult<ProfileUpdate> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let index = remote_profile_index(&catalog, id)?;
        if &catalog.profiles[index] != expected_record {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {id} 在订阅下载期间已改变，拒绝覆盖较新版本"
            )));
        }
        let path = self.profile_path(&catalog.profiles[index]);
        let previous_record = catalog.profiles[index].clone();
        let previous_payload = read_profile_bytes(&path)?;
        atomic_write(&path, &applied_payload)?;
        catalog.profiles[index].updated_at = super::unix_timestamp();
        catalog.profiles[index].size_bytes = applied_payload.len() as u64;
        catalog.profiles[index]
            .subscription
            .merge_from(subscription);
        let accepts_suggested_interval = matches!(
            &catalog.profiles[index].source,
            ProfileSource::Remote { options, .. } if !options.fixed_update_interval
        );
        if let Some(interval) = accepts_suggested_interval
            .then_some(
                catalog.profiles[index]
                    .subscription
                    .suggested_update_interval_minutes,
            )
            .flatten()
        {
            catalog.profiles[index].update_interval_minutes = interval;
        }
        let record = catalog.profiles[index].clone();
        if let Err(error) = self.save_unlocked(&catalog) {
            return match atomic_write(&path, &previous_payload) {
                Ok(()) => Err(error),
                Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                    "保存订阅索引失败：{error}；恢复上一版本配置失败：{rollback}"
                ))),
            };
        }
        Ok(ProfileUpdate {
            record,
            previous_record,
            previous_payload,
            applied_payload,
        })
    }

    /// Restores a remote update if neither its metadata nor payload changed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale update token or a failed rollback.
    pub fn rollback_update(&self, update: ProfileUpdate) -> ProfileStoreResult<ProfileRecord> {
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let index = catalog
            .profiles
            .iter()
            .position(|profile| profile.id == update.record.id)
            .ok_or_else(|| ProfileStoreError::NotFound(update.record.id.clone()))?;
        if catalog.profiles[index] != update.record {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {} 已再次更新，拒绝使用陈旧版本回滚",
                update.record.id
            )));
        }
        let path = self.profile_path(&update.previous_record);
        let current_payload = read_profile_bytes(&path)?;
        if current_payload != update.applied_payload {
            return Err(ProfileStoreError::Transaction(format!(
                "配置 {} 的文件内容已改变，拒绝覆盖较新内容",
                update.record.id
            )));
        }
        atomic_write(&path, &update.previous_payload)?;
        catalog.profiles[index] = update.previous_record.clone();
        if let Err(error) = self.save_unlocked(&catalog) {
            return match atomic_write(&path, &current_payload) {
                Ok(()) => Err(error),
                Err(rollback) => Err(ProfileStoreError::Transaction(format!(
                    "恢复订阅索引失败：{error}；重新应用当前配置失败：{rollback}"
                ))),
            };
        }
        Ok(update.previous_record)
    }
}

fn remote_profile(catalog: &ProfileCatalog, id: &str) -> ProfileStoreResult<ProfileRecord> {
    let profile = catalog
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
    match &profile.source {
        ProfileSource::Remote { .. } => Ok(profile.clone()),
        ProfileSource::Local { .. } => {
            Err(ProfileStoreError::NotFound(format!("{id} 不是在线订阅")))
        }
    }
}

fn remote_profile_index(catalog: &ProfileCatalog, id: &str) -> ProfileStoreResult<usize> {
    let index = catalog
        .profiles
        .iter()
        .position(|profile| profile.id == id)
        .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
    if !catalog.profiles[index].is_remote() {
        return Err(ProfileStoreError::NotFound(format!("{id} 不是在线订阅")));
    }
    Ok(index)
}

async fn run_store_task<T, F>(operation: F) -> ProfileStoreResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ProfileStoreResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            ProfileStoreError::Transaction(format!("配置仓库后台任务异常结束：{error}"))
        })?
}
