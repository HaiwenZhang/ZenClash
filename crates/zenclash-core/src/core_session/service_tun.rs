//! Admitted service handover; capture owns the outer completion and publication gate.

use super::*;
use crate::{owned_core::OwnedCore, service_runtime_session::ServiceRuntimeSession};

pub(crate) struct ServiceTunRuntimeOutcome {
    pub(crate) saved: Option<CoreApplyOutcome>,
    pub(crate) commit_pending: bool,
    pub(crate) recovery_warning: Option<String>,
    pub(crate) failure: Option<CoreSessionError>,
    pub(crate) restored: bool,
}

#[cfg(test)]
mod preparation_tests;

pub(crate) enum PreparedConfig {
    Local(Box<PreparedLocalConfig>),
    Service(Arc<PreparedServiceTun>),
}

impl PreparedConfig {
    pub(crate) fn requires_service(&self) -> bool {
        matches!(self, Self::Service(_))
    }

    pub(crate) fn into_profile_source(self) -> crate::profile::ProfileRuntimeSource {
        let (update, expected_cache) = match self {
            Self::Local(prepared) => (prepared.update, prepared.expected_cache),
            Self::Service(prepared) => (prepared.update.clone(), prepared.expected_cache.clone()),
        };
        crate::profile::ProfileRuntimeSource::Prepared(Box::new((update, expected_cache)))
    }
}

pub(crate) use super::configuration_application::PreparedLocalConfig;

#[derive(Clone)]
pub(crate) struct PreparedServiceTun {
    update: crate::ControlledConfigUpdate,
    previous: Arc<crate::ServiceRuntimeBundle>,
    expected_cache: Option<String>,
    expected_runtime: RuntimeConfigurationRevision,
    session_generation: Arc<AtomicU64>,
    pending_backup: Option<PendingBackupRestore>,
    expected_pending: Option<(u64, PathBuf)>,
    next_config: Option<(Arc<crate::ServiceRuntimeBundle>, CommittedConfig)>,
    store_root: PathBuf,
    pub(crate) profile_commit: Option<Arc<crate::profiles::application::ServiceProfileCommit>>,
    pub(crate) authorized_store: Option<ControlledConfigStore>,
}

impl PreparedServiceTun {
    pub(crate) fn expected_runtime(&self) -> RuntimeConfigurationRevision {
        self.expected_runtime
    }
}

impl CoreSession {
    pub(crate) async fn prepare_service_backup_config(
        &self,
        store: &ControlledConfigStore,
        expected: RuntimeConfigurationRevision,
        candidate: &crate::backup::BackupRuntimeCandidate,
    ) -> Result<(PreparedConfig, CoreRestoreSnapshot), CoreSessionError> {
        let client = self.client.pin_binding()?;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(MihomoError::StaleBinding.into()),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let _transition = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let read = store.clone();
        let session = self.clone();
        let worker_candidate = candidate.clone();
        let (update, mut previous) = tokio::task::spawn_blocking(move || {
            let previous = session.capture_restore_snapshot(&read)?;
            let payload = previous.payload.clone().ok_or_else(|| {
                ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                ))
            })?;
            let update = read.prepare_backup_config_update(&worker_candidate, payload)?;
            Ok::<_, CoreSessionError>((update, previous))
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let committed = CommittedConfig {
            profile: Some(candidate.profile.clone()),
            overrides: candidate.overrides.clone(),
        };
        let previous_bundle = Arc::new(
            crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone()).await?,
        );
        previous.service_bundle = Some(previous_bundle.clone());
        let prepared = if crate::tun_admission::yaml_enables_tun(update.next_payload())?
            && !crate::current_process_elevated()
        {
            let next =
                Arc::new(crate::ServiceRuntimeBundle::prepare(update.next_payload(), home).await?);
            PreparedConfig::Service(Arc::new(PreparedServiceTun {
                update,
                previous: previous_bundle,
                expected_cache: None,
                expected_runtime: expected,
                session_generation: self.generation.clone(),
                pending_backup: None,
                expected_pending: self.service_tun_pending_identity(),
                next_config: Some((next, committed)),
                store_root: store.root().to_path_buf(),
                profile_commit: None,
                authorized_store: None,
            }))
        } else {
            PreparedConfig::Local(Box::new(PreparedLocalConfig {
                update,
                expected_cache: None,
                expected_runtime: expected,
                session_generation: self.generation.clone(),
                store_root: store.root().to_path_buf(),
                committed,
                persist_patch: false,
            }))
        };
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        Ok((prepared, previous))
    }

    pub(crate) fn validate_backup_snapshot(
        expected: &CoreRestoreSnapshot,
        actual: &CoreRestoreSnapshot,
    ) -> Result<(), CoreSessionError> {
        if expected.payload != actual.payload
            || expected.committed.profile != actual.committed.profile
            || expected.committed.overrides != actual.committed.overrides
        {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }
    pub(crate) async fn prepare_service_tun(
        &self,
        store: &ControlledConfigStore,
        expected: RuntimeConfigurationRevision,
        fallback_profile: Option<PathBuf>,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
    ) -> Result<Arc<PreparedServiceTun>, CoreSessionError> {
        // Explicit TUN handover prepares privileged bytes while the current
        // binding still points to the ordinary child with its TUN-off projection.
        let store = store.for_service_runtime();
        let client = self.client.pin_binding()?;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let committed = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let profile = committed
            .profile
            .clone()
            .or(fallback_profile)
            .ok_or(CoreSessionError::NoCommittedProfile)?;
        let read = store.clone();
        let expected_cache = tokio::task::spawn_blocking(move || read.cached_runtime_payload())
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let update = if let Some(bundle) = recovery_bundle.as_ref() {
            store
                .prepare_service_tun_recovery_update(bundle.clone())
                .await?
        } else {
            store
                .prepare_service_tun_update(profile, committed.overrides.clone(), None)
                .await?
        };
        let previous = match recovery_bundle {
            Some(bundle) => bundle,
            None => Arc::new(
                crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone())
                    .await?,
            ),
        };
        let delta = serde_json::json!({"tun":{"enable":true}});
        self.validate_backup_delta(&delta)?;
        let expected_pending = self.service_tun_pending_identity();
        let pending_backup = self
            .prepare_service_tun_backup(&home, update.previous_payload(), &previous, &delta)
            .await?;
        if self.service_tun_pending_identity() != expected_pending {
            return Err(MihomoError::StaleBinding.into());
        }
        store
            .validate_prepared_service_tun(update.clone(), expected_cache.clone())
            .await?;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        Ok(Arc::new(PreparedServiceTun {
            update,
            previous,
            expected_cache,
            expected_runtime: expected,
            session_generation: self.generation.clone(),
            pending_backup,
            expected_pending,
            next_config: None,
            store_root: store.root().to_path_buf(),
            profile_commit: None,
            authorized_store: None,
        }))
    }

    pub(crate) async fn prepare_service_config(
        &self,
        store: &ControlledConfigStore,
        expected: RuntimeConfigurationRevision,
        intent: EffectiveConfigIntent,
    ) -> Result<PreparedConfig, CoreSessionError> {
        self.prepare_service_config_source(store, expected, intent, None)
            .await
    }

    pub(crate) async fn prepare_service_profile_config(
        &self,
        store: &ControlledConfigStore,
        expected: RuntimeConfigurationRevision,
        profile: PathBuf,
        payload: Arc<str>,
        overrides: Vec<PathBuf>,
    ) -> Result<PreparedConfig, CoreSessionError> {
        self.prepare_service_config_source(
            store,
            expected,
            EffectiveConfigIntent::ActivateProfile { profile, overrides },
            Some(crate::profile::ProfileRuntimeSource::Frozen(payload)),
        )
        .await
    }

    async fn prepare_service_config_source(
        &self,
        store: &ControlledConfigStore,
        expected: RuntimeConfigurationRevision,
        intent: EffectiveConfigIntent,
        source: Option<crate::profile::ProfileRuntimeSource>,
    ) -> Result<PreparedConfig, CoreSessionError> {
        let client = self.client.pin_binding()?;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let home = match client.owned_core() {
            Some(OwnedCore::Local(process)) => process.launch_config().home_dir.clone(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let lease = store
            .acquire_write_lease_for_paths(client.write_scopes())
            .await?;
        let store = store.with_write_lease(&lease);
        let committed = self.transition.clone().lock_owned().await;
        let _mutation = store.lock_service_tun_mutation().await;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        let (profile, overrides, patch) = match intent {
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                (profile, overrides, None)
            }
            EffectiveConfigIntent::ReapplyCurrent { overrides } => (
                committed
                    .profile
                    .clone()
                    .ok_or(CoreSessionError::NoCommittedProfile)?,
                overrides,
                None,
            ),
            EffectiveConfigIntent::Patch {
                profile,
                overrides,
                patch,
            } => (
                committed.profile.clone().unwrap_or(profile),
                if committed.profile.is_some() {
                    committed.overrides.clone()
                } else {
                    overrides
                },
                Some(patch),
            ),
        };
        let read = store.clone();
        let expected_cache = tokio::task::spawn_blocking(move || read.cached_runtime_payload())
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()))??;
        let persist_patch = patch.is_some();
        let update = store
            .prepare_service_config_update(
                source.unwrap_or_else(|| profile.clone().into()),
                patch,
                overrides.clone(),
                expected_cache.clone(),
            )
            .await?;
        if !crate::tun_admission::yaml_enables_tun(update.next_payload())?
            || crate::current_process_elevated()
        {
            store
                .validate_prepared_service_tun(update.clone(), expected_cache.clone())
                .await?;
            self.verify_prepared_mihomo_runtime(&client, expected)?;
            return Ok(PreparedConfig::Local(Box::new(PreparedLocalConfig {
                update,
                expected_cache,
                expected_runtime: expected,
                session_generation: self.generation.clone(),
                store_root: store.root().to_path_buf(),
                committed: CommittedConfig {
                    profile: Some(profile),
                    overrides,
                },
                persist_patch,
            })));
        }
        if expected_cache.is_none() {
            return Err(ControlledConfigError::Transaction(zenclash_i18n::text(
                "core_page.service.no_snapshot",
            ))
            .into());
        }
        let previous = Arc::new(
            crate::ServiceRuntimeBundle::prepare(update.previous_payload(), home.clone()).await?,
        );
        let next =
            Arc::new(crate::ServiceRuntimeBundle::prepare(update.next_payload(), home).await?);
        let expected_pending = self.service_tun_pending_identity();
        store
            .validate_prepared_service_tun(update.clone(), expected_cache.clone())
            .await?;
        self.verify_prepared_mihomo_runtime(&client, expected)?;
        Ok(PreparedConfig::Service(Arc::new(PreparedServiceTun {
            update,
            previous,
            expected_cache,
            expected_runtime: expected,
            session_generation: self.generation.clone(),
            pending_backup: None,
            expected_pending,
            next_config: Some((
                next,
                CommittedConfig {
                    profile: Some(profile),
                    overrides,
                },
            )),
            store_root: store.root().to_path_buf(),
            profile_commit: None,
            authorized_store: None,
        })))
    }

    pub(crate) async fn enable_service_tun_admitted(
        &self,
        store: &ControlledConfigStore,
        service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        fallback_profile: Option<PathBuf>,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        prepared_config: Option<Arc<PreparedServiceTun>>,
    ) -> Result<ServiceTunRuntimeOutcome, CoreSessionError> {
        let before = self.client.pin_binding()?;
        self.verify_prepared_mihomo_runtime(&before, expected_runtime)?;
        let old = before
            .owned_core()
            .ok_or(CoreSessionError::ReleaseUnsupported { core: self.kind })?;
        let source_home = match (&old, &service) {
            (OwnedCore::Local(process), Some((_, home)))
                if process.launch_config().home_dir == *home =>
            {
                home.clone()
            }
            (OwnedCore::Service(runtime), None) => runtime.source_home().to_path_buf(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let mut scopes = before.write_scopes();
        scopes.push(source_home.clone());
        let profile_commit = prepared_config
            .as_ref()
            .and_then(|prepared| prepared.profile_commit.clone());
        if let Some(profile) = &profile_commit {
            scopes.push(profile.root().to_path_buf());
        }
        let lease = store.acquire_write_lease_for_paths(scopes).await?;
        self.verify_prepared_mihomo_runtime(&before, expected_runtime)?;
        let store = store.with_write_lease(&lease);
        let mut committed = self.transition.clone().lock_owned().await;
        self.verify_prepared_mihomo_runtime(&before, expected_runtime)?;
        let _store_mutation = store.lock_service_tun_mutation().await;
        self.verify_prepared_mihomo_runtime(&before, expected_runtime)?;
        let mut mutation = before.lock_runtime_binding().await?;
        self.verify_prepared_mihomo_runtime(&before, expected_runtime)?;
        let _availability_pause = self.pause_pac_availability();
        let result = async {
            if let Some(profile) = &profile_commit {
                profile.validate(&lease).await?;
            }
            let next_config = prepared_config
                .as_ref()
                .and_then(|prepared| prepared.next_config.clone());
            let prepared_config_is_full = next_config.is_some();
            let export_local_caches = recovery_bundle.is_some();
            let profile = next_config
                .as_ref()
                .and_then(|(_, config)| config.profile.clone())
                .or_else(|| committed.profile.clone())
                .or(fallback_profile)
                .ok_or(CoreSessionError::NoCommittedProfile)?;
            let held = match &old {
                OwnedCore::Service(runtime) if recovery_bundle.is_none() => {
                    Some(runtime.snapshot()?)
                }
                OwnedCore::Service(_) => return Err(MihomoError::StaleBinding.into()),
                OwnedCore::Local(_) => recovery_bundle.clone(),
            };
            let update = if let Some(prepared) = prepared_config.as_ref() {
                if prepared.store_root != store.root()
                    || !Arc::ptr_eq(&prepared.session_generation, &self.generation)
                    || prepared.expected_runtime != expected_runtime
                {
                    return Err(MihomoError::StaleBinding.into());
                }
                store
                    .validate_prepared_service_tun(
                        prepared.update.clone(),
                        prepared.expected_cache.clone(),
                    )
                    .await?;
                prepared.update.clone()
            } else if let Some(bundle) = recovery_bundle {
                store.prepare_service_tun_recovery_update(bundle).await?
            } else {
                store
                    .prepare_service_tun_update(
                        profile.clone(),
                        committed.overrides.clone(),
                        held.clone(),
                    )
                    .await?
            };
            let previous_bundle = match (prepared_config.as_ref(), held) {
                (Some(prepared), _) => prepared.previous.clone(),
                (None, Some(bundle)) => bundle,
                (None, None) => Arc::new(
                    crate::ServiceRuntimeBundle::prepare(
                        update.previous_payload(),
                        source_home.clone(),
                    )
                    .await?,
                ),
            };
            let delta = serde_json::json!({"tun":{"enable":true}});
            if !prepared_config_is_full {
                self.validate_backup_delta(&delta)?;
            }
            let pending_backup = if let Some(prepared) = prepared_config {
                if self.service_tun_pending_identity() != prepared.expected_pending {
                    return Err(MihomoError::StaleBinding.into());
                }
                prepared.pending_backup.clone()
            } else {
                self.prepare_service_tun_backup(
                    &source_home,
                    update.previous_payload(),
                    &previous_bundle,
                    &delta,
                )
                .await?
            };
            let next_bundle = if let Some((bundle, _)) = next_config.as_ref() {
                bundle.clone()
            } else {
                Arc::new(previous_bundle.with_delta(&delta)?)
            };
            let runtime = match (&old, service) {
                (OwnedCore::Service(runtime), None) => runtime.clone(),
                (OwnedCore::Local(process), Some((client, _))) => {
                    ServiceRuntimeSession::from_local(client, process.launch_config().clone())
                }
                _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
            };
            let mut prepared = runtime.prepare_bundle(next_bundle).await?;
            let persistence = store.stage_service_tun_update(update).await?;
            if let Some(profile) = &profile_commit {
                profile.mark_runtime_attempted();
            }
            if let OwnedCore::Local(process) = &old {
                // A is reaped before B can start. The exact B owner is published before Start,
                // so an uncertain acknowledgement cannot hide a kernel from shutdown.
                if let Err(error) = process.stop_async().await {
                    drop(prepared);
                    let _cache = persistence.rollback().await;
                    let _release = runtime.release_owned().await;
                    return Ok(self.failed_service_tun(error.into(), false));
                }
                if export_local_caches {
                    // The stopped recovery slot is authoritative for downloaded HTTP caches.
                    // Restage before Start; TLS and GeoData remain the held accepted bytes.
                    drop(prepared);
                    let refreshed = async {
                        let bundle = previous_bundle
                            .export_local_caches(
                                &store,
                                process.launch_config().config_file.clone(),
                            )
                            .await?;
                        let bundle = Arc::new(bundle.with_delta(&delta)?);
                        runtime.prepare_bundle(bundle).await
                    }
                    .await;
                    prepared = match refreshed {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let cache = persistence.rollback().await;
                            let restored = self
                                .recover_service_tun(
                                    &old,
                                    &runtime,
                                    (&store, _store_mutation),
                                    &previous_bundle,
                                    mutation,
                                    cache.is_ok(),
                                )
                                .await;
                            return Ok(self.failed_service_tun(error.into(), restored));
                        }
                    };
                }
                if let Err(error) = self.ensure_not_shutting_down() {
                    drop(prepared);
                    let cache = persistence.rollback().await;
                    let recovered = self
                        .recover_service_tun(
                            &old,
                            &runtime,
                            (&store, _store_mutation),
                            &previous_bundle,
                            mutation,
                            cache.is_ok(),
                        )
                        .await;
                    return Ok(self.failed_service_tun(error, recovered));
                }
                match self
                    .client
                    .publish_prepared_service(runtime.clone(), mutation)
                    .await
                {
                    Ok(guard) => mutation = guard,
                    Err(error) => {
                        drop(prepared);
                        let cache = persistence.rollback().await;
                        // Publication may have succeeded before retirement reported a failure.
                        // Reacquire the current mutation gate before inspecting or restoring it.
                        let recovered = match self.client.lock_runtime_binding().await {
                            Ok(guard) => {
                                self.recover_service_tun(
                                    &old,
                                    &runtime,
                                    (&store, _store_mutation),
                                    &previous_bundle,
                                    guard,
                                    cache.is_ok(),
                                )
                                .await
                            }
                            Err(_) => {
                                let _release = runtime.release_owned().await;
                                false
                            }
                        };
                        return Ok(self.failed_service_tun(error.into(), recovered));
                    }
                }
            }
            let applied = prepared.apply(true).await;
            let applied = match applied {
                Ok(applied) => applied,
                Err(error) => {
                    let cache = persistence.rollback().await;
                    let recovered = self
                        .recover_service_tun(
                            &old,
                            &runtime,
                            (&store, _store_mutation),
                            &previous_bundle,
                            mutation,
                            cache.is_ok(),
                        )
                        .await;
                    return Ok(self.failed_service_tun(error.into(), recovered));
                }
            };
            let live = match self.client.pin_binding() {
                Ok(client) => client.runtime_config().await,
                Err(error) => Err(error),
            };
            let verified = live.as_ref().is_ok_and(|config| config.tun.enable);
            if !verified || self.is_shutting_down() {
                drop(applied);
                let cache = persistence.rollback().await;
                let recovered = self
                    .recover_service_tun(
                        &old,
                        &runtime,
                        (&store, _store_mutation),
                        &previous_bundle,
                        mutation,
                        cache.is_ok(),
                    )
                    .await;
                return Ok(self.failed_service_tun(
                    live.err().map(CoreSessionError::from).unwrap_or_else(|| {
                        CoreSessionError::Process(MihomoError::Process(zenclash_i18n::text(
                            "core_page.service.unknown",
                        )))
                    }),
                    recovered,
                ));
            }
            let save = if let Some(profile) = &profile_commit {
                // A directory application must not rewrite the controlled layer.
                // Both writes remain rollbackable until the catalog compare-commit succeeds.
                match persistence.validate_patch().await {
                    Ok(()) => profile.commit(&lease).await,
                    Err(error) => Err(error.into()),
                }
            } else {
                persistence.save().await.map_err(CoreSessionError::from)
            };
            if let Err(error) = save {
                drop(applied);
                let cache = persistence.rollback().await;
                let recovered = self
                    .recover_service_tun(
                        &old,
                        &runtime,
                        (&store, _store_mutation),
                        &previous_bundle,
                        mutation,
                        cache.is_ok(),
                    )
                    .await;
                return Ok(self.failed_service_tun(error, recovered));
            }
            persistence.saved();
            let (generation, pending_conflict) = if let Some((_, next_committed)) = next_config {
                *committed = next_committed;
                (
                    self.next_generation_with_config(Some(committed.clone())),
                    false,
                )
            } else {
                committed.profile = Some(profile);
                self.accept_saved_service_tun(committed.clone(), pending_backup)
            };
            let saved = CoreApplyOutcome {
                kind: if prepared_config_is_full {
                    CoreApplyKind::HotReloaded
                } else {
                    CoreApplyKind::Patched
                },
                generation,
            };
            // Once the override is durable, retain its receipt and finish the same native candidate.
            // Even an in-memory publication error must not drop a saved candidate back to Applied.
            let confirmation = applied.commit().await.map_err(CoreSessionError::from);
            let commit_pending = confirmation.is_err();
            let recovery_warning =
                pending_conflict.then(|| MihomoError::StaleTransport.to_string());
            let failure = pending_conflict
                .then(|| CoreSessionError::Process(MihomoError::StaleTransport))
                .or_else(|| confirmation.err());
            drop(mutation);
            self.lifecycle.write().phase = if failure.is_none() {
                CoreLifecyclePhase::Stable
            } else {
                CoreLifecyclePhase::Unknown
            };
            Ok(ServiceTunRuntimeOutcome {
                saved: Some(saved),
                commit_pending,
                recovery_warning,
                failure,
                restored: false,
            })
        }
        .await;
        if let Some(profile) = &profile_commit {
            // Capture still owns publication, so this is this transaction's version.
            profile.record_runtime_generation(self.generation());
        }
        // A stopped local owner may be the last Arc once B is published. Retire
        // both old pins away from async workers while transition/capture still own admission.
        let retirement = tokio::task::spawn_blocking(move || drop((old, before)))
            .await
            .map_err(|error| ControlledConfigError::Task(error.to_string()));
        self.finish_service_tun_retirement(result, retirement)
    }

    fn finish_service_tun_retirement(
        &self,
        result: Result<ServiceTunRuntimeOutcome, CoreSessionError>,
        retirement: Result<(), ControlledConfigError>,
    ) -> Result<ServiceTunRuntimeOutcome, CoreSessionError> {
        let Err(error) = retirement else {
            return result;
        };
        tracing::warn!(%error, "failed to retire previous core binding after service TUN completion");
        self.lifecycle.write().phase = CoreLifecyclePhase::Unknown;
        result.map(|mut outcome| {
            let warning = zenclash_i18n::text("core_page.service.cleanup_unconfirmed");
            if let Some(previous) = &mut outcome.recovery_warning {
                previous.push_str("; ");
                previous.push_str(&warning);
            } else {
                outcome.recovery_warning = Some(warning);
            }
            if outcome.failure.is_none() {
                outcome.failure = Some(CoreSessionError::PreviousCoreCleanupUnconfirmed);
            }
            outcome.restored = false;
            outcome
        })
    }

    async fn prepare_service_tun_backup(
        &self,
        home: &std::path::Path,
        previous_payload: &str,
        bundle: &Arc<crate::ServiceRuntimeBundle>,
        delta: &serde_json::Value,
    ) -> Result<Option<PendingBackupRestore>, CoreSessionError> {
        let Some(mut pending) = self.pending_backup.read().clone() else {
            return Ok(None);
        };
        if pending.snapshot.service_bundle.is_none() {
            let payload = pending.snapshot.payload.as_deref().ok_or_else(|| {
                ControlledConfigError::Transaction(zenclash_i18n::text(
                    "core_page.service.no_snapshot",
                ))
            })?;
            pending.snapshot.service_bundle = Some(if payload == previous_payload {
                bundle.clone()
            } else {
                Arc::new(crate::ServiceRuntimeBundle::prepare(payload, home.to_path_buf()).await?)
            });
        }
        pending.snapshot = snapshot_with_delta(&pending.snapshot, delta)?;
        Ok(Some(pending))
    }

    fn service_tun_pending_identity(&self) -> Option<(u64, PathBuf)> {
        self.pending_backup
            .read()
            .as_ref()
            .map(|pending| (pending.generation, pending.store_root.clone()))
    }

    fn accept_saved_service_tun(
        &self,
        config: CommittedConfig,
        prepared: Option<PendingBackupRestore>,
    ) -> (u64, bool) {
        // Every fallible resource/YAML operation was completed before A stopped.
        // Publication of the durable receipt must never roll back the saved cache.
        let mut committed = self.committed_profile.write();
        let mut pending = self.pending_backup.write();
        let matches = match (pending.as_ref(), prepared.as_ref()) {
            (None, None) => true,
            (Some(current), Some(prepared)) => {
                current.generation == prepared.generation
                    && current.store_root == prepared.store_root
            }
            _ => false,
        };
        if matches {
            *pending = prepared;
        }
        committed.config = config;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        committed.generation = generation;
        if let Some(pending) = pending.as_mut() {
            pending.generation = generation;
        }
        self.client.invalidate_connections();
        (generation, !matches)
    }

    async fn recover_service_tun(
        &self,
        old: &OwnedCore,
        runtime: &Arc<ServiceRuntimeSession>,
        admission: (&ControlledConfigStore, tokio::sync::OwnedMutexGuard<()>),
        previous_bundle: &Arc<crate::ServiceRuntimeBundle>,
        mutation: tokio::sync::OwnedMutexGuard<()>,
        cache_restored: bool,
    ) -> bool {
        let (store, store_mutation) = admission;
        if let OwnedCore::Local(process) = old {
            // Release includes confirmed Stop. An unknown result keeps B bound;
            // restoring A would risk two simultaneous TUN kernels.
            if runtime.release_owned().await.is_err() || !cache_restored || self.is_shutting_down()
            {
                return false;
            }
            let client = self.client.clone();
            runtime
                .recover_held_local_runtime(
                    (store, store_mutation),
                    process.launch_config().clone(),
                    previous_bundle.clone(),
                    self.shutdown_requested.clone(),
                    CORE_READY_TIMEOUT,
                    move |process| async move {
                        client.publish_prepared_process(process, mutation).await
                    },
                )
                .await
                .is_ok_and(|outcome| outcome.failure.is_none())
        } else {
            let _mutation = mutation;
            cache_restored && runtime.restore_active().await.is_ok()
        }
    }

    fn failed_service_tun(
        &self,
        failure: CoreSessionError,
        restored: bool,
    ) -> ServiceTunRuntimeOutcome {
        self.next_generation();
        self.lifecycle.write().phase = if restored {
            CoreLifecyclePhase::Stable
        } else {
            CoreLifecyclePhase::Unknown
        };
        ServiceTunRuntimeOutcome {
            saved: None,
            commit_pending: false,
            recovery_warning: None,
            failure: Some(failure),
            restored,
        }
    }
}

#[cfg(test)]
mod tests;
