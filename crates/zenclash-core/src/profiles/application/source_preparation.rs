//! Local/edit/download source preparation for the original profile transaction owner.

use super::*;

impl ProfileApplication {
    pub(super) async fn stage_change(
        &self,
        change: ProfileChange,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        match change {
            ProfileChange::ImportLocal { source, overrides } => {
                self.import_local(source, overrides).await
            }
            ProfileChange::AddRemote {
                name,
                url,
                user_agent,
                options,
                overrides,
            } => {
                self.add_remote(name, url, user_agent, options, overrides)
                    .await
            }
            ProfileChange::UpdateRemote { id, overrides } => {
                self.update_remote(id, overrides).await
            }
            ProfileChange::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            } => {
                self.edit_yaml(id, expected_payload, new_payload, overrides)
                    .await
            }
            ProfileChange::ActivateExisting { id, overrides } => {
                self.activate_existing(id, overrides).await
            }
        }
    }

    async fn import_local(
        &self,
        source: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(metadata) => metadata,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let source_candidate =
            match run_store(move || ProfileSourceCandidate::local(store, &source)).await {
                Ok(source_candidate) => source_candidate,
                Err(cause) => {
                    return Err(ProfilePreparationError {
                        last_known_good,
                        cause,
                    });
                }
            };
        Ok((source_candidate, overrides))
    }

    async fn update_remote(
        &self,
        id: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        let store = self.store.clone();
        let lookup_id = id.clone();
        let (expected_record, last_known_good) = match run_store(move || {
            let catalog = store.load()?;
            let record = catalog
                .profiles
                .iter()
                .find(|profile| profile.id == lookup_id)
                .cloned()
                .ok_or_else(|| ProfileStoreError::NotFound(lookup_id))?;
            if !record.is_remote() {
                return Err(ProfileStoreError::NotFound(format!(
                    "{} 不是在线订阅",
                    record.id
                )));
            }
            let last_known_good = catalog.active_profile().map(ProfileSourceMetadata::from);
            Ok((record, last_known_good))
        })
        .await
        {
            Ok(prepared) => prepared,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &expected_record.source
        else {
            return Err(ProfilePreparationError {
                last_known_good,
                cause: ProfileStoreError::NotFound(format!("{id} 不是在线订阅")).into(),
            });
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        let downloaded = match download_profile(url, user_agent, options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let store = self.store.clone();
        let source_candidate = match run_store(move || {
            ProfileSourceCandidate::remote_update(
                store,
                &expected_record,
                downloaded.payload,
                downloaded.metadata,
            )
        })
        .await
        {
            Ok(source_candidate) => source_candidate,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((source_candidate, overrides))
    }

    async fn edit_yaml(
        &self,
        id: String,
        expected_payload: String,
        new_payload: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(metadata) => metadata,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let source_candidate = match run_store(move || {
            ProfileSourceCandidate::edit(store, &id, &expected_payload, new_payload)
        })
        .await
        {
            Ok(source_candidate) => source_candidate,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((source_candidate, overrides))
    }

    async fn add_remote(
        &self,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
        overrides: Vec<PathBuf>,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(metadata) => metadata,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let name = match if name.trim().is_empty() {
            Ok(String::new())
        } else {
            normalized_profile_name(&name)
        } {
            Ok(name) => name,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let url = match normalized_remote_url(&url) {
            Ok(url) => url,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let user_agent = match normalized_user_agent(&user_agent) {
            Ok(user_agent) => user_agent,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let proxy_port = match self.subscription_proxy_port(options.route()).await {
            Ok(port) => port,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        let downloaded = match download_profile(&url, &user_agent, &options, proxy_port).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause: error.into(),
                });
            }
        };
        let name = if name.is_empty() {
            downloaded.suggested_name.clone()
        } else {
            name
        };
        let source = ProfileSource::Remote {
            url,
            user_agent,
            options,
        };
        let store = self.store.clone();
        let source_candidate = match run_store(move || {
            ProfileSourceCandidate::new(
                store,
                name,
                source,
                downloaded.payload,
                downloaded.metadata,
            )
        })
        .await
        {
            Ok(source_candidate) => source_candidate,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok((source_candidate, overrides))
    }

    async fn activate_existing(
        &self,
        id: String,
        overrides: Vec<PathBuf>,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        let last_known_good = match self.last_known_good().await {
            Ok(metadata) => metadata,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good: None,
                    cause,
                });
            }
        };
        let store = self.store.clone();
        let source_candidate =
            match run_store(move || ProfileSourceCandidate::existing(store, &id)).await {
                Ok(source_candidate) => source_candidate,
                Err(cause) => {
                    return Err(ProfilePreparationError {
                        last_known_good,
                        cause,
                    });
                }
            };
        Ok((source_candidate, overrides))
    }

    pub(super) async fn last_known_good(
        &self,
    ) -> Result<Option<ProfileSourceMetadata>, ProfileApplicationError> {
        let store = self.store.clone();
        run_store(move || store.load())
            .await
            .map(|catalog| catalog.active_profile().map(ProfileSourceMetadata::from))
    }

    async fn subscription_proxy_port(
        &self,
        route: RemoteProfileRoute,
    ) -> Result<Option<u16>, ProfileApplicationError> {
        if route == RemoteProfileRoute::Direct {
            return Ok(None);
        }
        let config = match self.session.client().runtime_config().await {
            Ok(config) => config,
            Err(_) if route == RemoteProfileRoute::DirectWithMihomoFallback => return Ok(None),
            Err(error) => return Err(error.into()),
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
            Err(MihomoError::InvalidInput(
                "当前内核没有可供订阅下载使用的 HTTP 或 Mixed 端口".into(),
            )
            .into())
        }
    }
}
