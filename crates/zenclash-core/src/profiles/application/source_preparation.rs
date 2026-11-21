//! Local/edit/download source preparation for the original profile transaction owner.

use super::*;

pub(super) struct DownloadedNewProfile {
    name: String,
    source: ProfileSource,
    payload: String,
    metadata: SubscriptionMetadata,
    last_known_good: Option<ProfileSourceMetadata>,
    overrides: Vec<PathBuf>,
}

pub(super) struct DownloadedUpdateProfile {
    expected_record: ProfileRecord,
    payload: String,
    metadata: SubscriptionMetadata,
    last_known_good: Option<ProfileSourceMetadata>,
    overrides: Vec<PathBuf>,
}

pub(super) enum PreparedSourceInput {
    ImportLocal {
        source: PathBuf,
        overrides: Vec<PathBuf>,
    },
    DownloadedNew(Box<DownloadedNewProfile>),
    DownloadedUpdate(Box<DownloadedUpdateProfile>),
    EditYaml {
        id: String,
        expected_payload: String,
        new_payload: String,
        overrides: Vec<PathBuf>,
    },
    ActivateExisting {
        id: String,
        overrides: Vec<PathBuf>,
    },
}

impl PreparedSourceInput {
    pub(super) fn last_known_good(&self) -> Option<ProfileSourceMetadata> {
        match self {
            Self::DownloadedNew(downloaded) => downloaded.last_known_good.clone(),
            Self::DownloadedUpdate(downloaded) => downloaded.last_known_good.clone(),
            Self::ImportLocal { .. } | Self::EditYaml { .. } | Self::ActivateExisting { .. } => {
                None
            }
        }
    }
}

impl ProfileApplication {
    pub(super) async fn prepare_source_input(
        &self,
        change: ProfileChange,
    ) -> Result<PreparedSourceInput, ProfilePreparationError> {
        match change {
            ProfileChange::ImportLocal { source, overrides } => {
                Ok(PreparedSourceInput::ImportLocal { source, overrides })
            }
            ProfileChange::AddRemote {
                name,
                url,
                user_agent,
                options,
                overrides,
            } => {
                self.download_new_remote(name, url, user_agent, options, overrides)
                    .await
            }
            ProfileChange::UpdateRemote { id, overrides } => {
                self.download_remote_update(id, overrides).await
            }
            ProfileChange::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            } => Ok(PreparedSourceInput::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            }),
            ProfileChange::ActivateExisting { id, overrides } => {
                Ok(PreparedSourceInput::ActivateExisting { id, overrides })
            }
        }
    }

    pub(super) async fn stage_prepared_input(
        &self,
        input: PreparedSourceInput,
    ) -> Result<(ProfileSourceCandidate, Vec<PathBuf>), ProfilePreparationError> {
        match input {
            PreparedSourceInput::ImportLocal { source, overrides } => {
                self.import_local(source, overrides).await
            }
            PreparedSourceInput::DownloadedNew(downloaded) => {
                let DownloadedNewProfile {
                    name,
                    source,
                    payload,
                    metadata,
                    last_known_good,
                    overrides,
                } = *downloaded;
                let store = self.store.clone();
                let source_candidate = match run_store(move || {
                    ProfileSourceCandidate::new(store, name, source, payload, metadata)
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
            PreparedSourceInput::DownloadedUpdate(downloaded) => {
                let DownloadedUpdateProfile {
                    expected_record,
                    payload,
                    metadata,
                    last_known_good,
                    overrides,
                } = *downloaded;
                let store = self.store.clone();
                let source_candidate = match run_store(move || {
                    ProfileSourceCandidate::remote_update(
                        store,
                        &expected_record,
                        payload,
                        metadata,
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
            PreparedSourceInput::EditYaml {
                id,
                expected_payload,
                new_payload,
                overrides,
            } => {
                self.edit_yaml(id, expected_payload, new_payload, overrides)
                    .await
            }
            PreparedSourceInput::ActivateExisting { id, overrides } => {
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

    async fn download_remote_update(
        &self,
        id: String,
        overrides: Vec<PathBuf>,
    ) -> Result<PreparedSourceInput, ProfilePreparationError> {
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
        let payload = downloaded.payload;
        let payload = match run_store(move || {
            validate_clash_yaml(&payload)?;
            Ok(payload)
        })
        .await
        {
            Ok(payload) => payload,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        Ok(PreparedSourceInput::DownloadedUpdate(Box::new(
            DownloadedUpdateProfile {
                expected_record,
                payload,
                metadata: downloaded.metadata,
                last_known_good,
                overrides,
            },
        )))
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

    async fn download_new_remote(
        &self,
        name: String,
        url: String,
        user_agent: String,
        options: RemoteProfileOptions,
        overrides: Vec<PathBuf>,
    ) -> Result<PreparedSourceInput, ProfilePreparationError> {
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
        let payload = downloaded.payload;
        let payload = match run_store(move || {
            validate_clash_yaml(&payload)?;
            Ok(payload)
        })
        .await
        {
            Ok(payload) => payload,
            Err(cause) => {
                return Err(ProfilePreparationError {
                    last_known_good,
                    cause,
                });
            }
        };
        let name = if name.is_empty() {
            downloaded.suggested_name
        } else {
            name
        };
        let source = ProfileSource::Remote {
            url,
            user_agent,
            options,
        };
        Ok(PreparedSourceInput::DownloadedNew(Box::new(
            DownloadedNewProfile {
                name,
                source,
                payload,
                metadata: downloaded.metadata,
                last_known_good,
                overrides,
            },
        )))
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
