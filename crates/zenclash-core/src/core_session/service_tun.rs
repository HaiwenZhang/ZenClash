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

impl CoreSession {
    pub(crate) async fn enable_service_tun_admitted(
        &self,
        store: &ControlledConfigStore,
        service: Option<(Arc<zenclash_service::ServiceClient>, PathBuf)>,
        expected_binding: u64,
        expected_generation: u64,
        fallback_profile: Option<PathBuf>,
    ) -> Result<ServiceTunRuntimeOutcome, CoreSessionError> {
        let before = self.client.pin_binding()?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let old = before
            .owned_core()
            .ok_or(CoreSessionError::ReleaseUnsupported { core: self.kind })?;
        let source_home = match (&old, &service) {
            (OwnedCore::Local(_), Some((_, home))) => home.clone(),
            (OwnedCore::Service(runtime), None) => runtime.source_home().to_path_buf(),
            _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
        };
        let mut scopes = before.write_scopes();
        scopes.push(source_home.clone());
        let lease = store.acquire_write_lease_for_paths(scopes).await?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let store = store.with_write_lease(&lease);
        let mut committed = self.transition.clone().lock_owned().await;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let _store_mutation = store.lock_service_tun_mutation().await;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let mut mutation = before.lock_runtime_binding().await?;
        self.check_service_tun_admission(&before, expected_binding, expected_generation)?;
        let result = async {
            let profile = committed
                .profile
                .clone()
                .or(fallback_profile)
                .ok_or(CoreSessionError::NoCommittedProfile)?;
            let held = match &old {
                OwnedCore::Service(runtime) => Some(runtime.snapshot()?),
                OwnedCore::Local(_) => None,
            };
            let update = store
                .prepare_service_tun_update(
                    profile.clone(),
                    committed.overrides.clone(),
                    held.clone(),
                )
                .await?;
            let previous_bundle = match held {
                Some(bundle) => bundle,
                None => Arc::new(
                    crate::ServiceRuntimeBundle::prepare(
                        update.previous_payload(),
                        source_home.clone(),
                    )
                    .await?,
                ),
            };
            let delta = serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}});
            self.validate_backup_delta(&delta)?;
            let pending_backup = self
                .prepare_service_tun_backup(
                    &source_home,
                    update.previous_payload(),
                    &previous_bundle,
                    &delta,
                )
                .await?;
            let next_bundle = Arc::new(previous_bundle.with_delta(&delta)?);
            let runtime = match (&old, service) {
                (OwnedCore::Service(runtime), None) => runtime.clone(),
                (OwnedCore::Local(_), Some((client, home))) => {
                    ServiceRuntimeSession::new(client, home)
                }
                _ => return Err(CoreSessionError::ReleaseUnsupported { core: self.kind }),
            };
            let prepared = runtime.prepare_bundle(next_bundle).await?;
            let persistence = store.stage_service_tun_update(update).await?;
            let initial = matches!(old, OwnedCore::Local(_));
            if let OwnedCore::Local(process) = &old {
                // A is reaped before B can start. The exact B owner is published before Start,
                // so an uncertain acknowledgement cannot hide a kernel from shutdown.
                if let Err(error) = process.stop_async().await {
                    drop(prepared);
                    let _cache = persistence.rollback().await;
                    let _release = runtime.release_owned().await;
                    return Ok(self.failed_service_tun(error.into(), false));
                }
                if let Err(error) = self.ensure_not_shutting_down() {
                    drop(prepared);
                    let cache = persistence.rollback().await;
                    let recovered = self
                        .recover_service_tun(
                            &old,
                            &runtime,
                            initial,
                            &lease,
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
                                    initial,
                                    &lease,
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
                            initial,
                            &lease,
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
                    .recover_service_tun(&old, &runtime, initial, &lease, mutation, cache.is_ok())
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
            if let Err(error) = persistence.save().await {
                drop(applied);
                let cache = persistence.rollback().await;
                let recovered = self
                    .recover_service_tun(&old, &runtime, initial, &lease, mutation, cache.is_ok())
                    .await;
                return Ok(self.failed_service_tun(error.into(), recovered));
            }
            persistence.saved();
            committed.profile = Some(profile);
            let (generation, pending_conflict) =
                self.accept_saved_service_tun(committed.clone(), pending_backup);
            let saved = CoreApplyOutcome {
                kind: CoreApplyKind::Patched,
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

    fn check_service_tun_admission(
        &self,
        client: &MihomoClient,
        binding: u64,
        generation: u64,
    ) -> Result<(), CoreSessionError> {
        client.ensure_binding_current()?;
        self.ensure_running_operations_allowed()?;
        if self.kind != CoreKind::Mihomo
            || self.runtime_descriptor().binding_generation() != binding
            || self.generation() != generation
        {
            return Err(MihomoError::StaleBinding.into());
        }
        Ok(())
    }

    async fn recover_service_tun(
        &self,
        old: &OwnedCore,
        runtime: &Arc<ServiceRuntimeSession>,
        initial: bool,
        lease: &DataWriteLease,
        mutation: tokio::sync::OwnedMutexGuard<()>,
        cache_restored: bool,
    ) -> bool {
        if initial {
            // Release includes confirmed Stop. An unknown result keeps B bound;
            // restoring A would risk two simultaneous TUN kernels.
            if runtime.release_owned().await.is_err() || !cache_restored || self.is_shutting_down()
            {
                return false;
            }
            let OwnedCore::Local(process) = old else {
                return false;
            };
            let Ok(_mutation) = self
                .client
                .publish_prepared_process(process.clone(), mutation)
                .await
            else {
                return false;
            };
            process
                .restart_and_wait_until_with_lease(
                    CORE_READY_TIMEOUT,
                    Some(self.shutdown_requested.clone()),
                    lease,
                )
                .await
                .is_ok()
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn service_tun_retirement_failure_keeps_saved_receipt_and_current_owner() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let binding = session.runtime_descriptor().binding_generation();
        let (generation, _) = session.accept_saved_service_tun(CommittedConfig::default(), None);
        let receipt = CoreApplyOutcome {
            kind: CoreApplyKind::Patched,
            generation,
        };
        let result = session.finish_service_tun_retirement(
            Ok(ServiceTunRuntimeOutcome {
                saved: Some(receipt),
                commit_pending: false,
                recovery_warning: None,
                failure: None,
                restored: false,
            }),
            Err(ControlledConfigError::Task(
                "retirement worker failed".into(),
            )),
        );
        assert!(result.is_ok(), "durable receipt became an ordinary error");
        let outcome = result.ok().unwrap();
        assert_eq!(outcome.saved, Some(receipt));
        assert!(!outcome.commit_pending);
        assert_eq!(
            outcome.recovery_warning,
            Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
        );
        assert_eq!(
            outcome.failure.unwrap().to_string(),
            zenclash_i18n::text("core_page.service.cleanup_unconfirmed")
        );
        assert!(!outcome.restored);
        assert_eq!(session.generation(), generation);
        assert_eq!(session.lifecycle.read().phase, CoreLifecyclePhase::Unknown);
        assert_eq!(session.runtime_descriptor().binding_generation(), binding);
    }

    #[tokio::test]
    async fn service_tun_retirement_warning_is_independent_of_pending_commit() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let outcome = session
            .finish_service_tun_retirement(
                Ok(ServiceTunRuntimeOutcome {
                    saved: Some(CoreApplyOutcome {
                        kind: CoreApplyKind::Patched,
                        generation: 1,
                    }),
                    commit_pending: true,
                    recovery_warning: None,
                    failure: Some(MihomoError::StaleTransport.into()),
                    restored: false,
                }),
                Err(ControlledConfigError::Task(
                    "retirement worker failed".into(),
                )),
            )
            .unwrap();
        assert!(outcome.commit_pending);
        assert_eq!(
            outcome.recovery_warning,
            Some(zenclash_i18n::text("core_page.service.cleanup_unconfirmed"))
        );
        assert!(matches!(
            outcome.failure,
            Some(CoreSessionError::Process(MihomoError::StaleTransport))
        ));
    }

    #[tokio::test]
    async fn service_tun_retirement_failure_preserves_original_unsaved_error() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let result = session.finish_service_tun_retirement(
            Err(MihomoError::StaleBinding.into()),
            Err(ControlledConfigError::Task(
                "retirement worker failed".into(),
            )),
        );
        assert!(matches!(
            result,
            Err(CoreSessionError::Process(MihomoError::StaleBinding))
        ));
    }

    #[tokio::test]
    async fn pending_service_tun_target_freezes_before_deletion_and_changes_only_after_save() {
        let home = std::env::temp_dir().join(format!(
            "zenclash-tun-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("rules.yaml"), "payload: [example.com]\n").unwrap();
        let payload = "mode: rule\ntun: {enable: false}\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: rules.yaml\n";
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        *session.pending_backup.write() = Some(PendingBackupRestore {
            snapshot: CoreRestoreSnapshot {
                committed: CommittedConfig::default(),
                payload: Some(payload.into()),
                service_bundle: None,
            },
            store_root: home.clone(),
            generation: 0,
        });
        let bundle = Arc::new(
            crate::ServiceRuntimeBundle::prepare(payload, home.clone())
                .await
                .unwrap(),
        );
        std::fs::remove_file(home.join("rules.yaml")).unwrap();
        let delta = serde_json::json!({"tun":{"enable":true},"dns":{"enable":true}});
        let prepared = session
            .prepare_service_tun_backup(&home, payload, &bundle, &delta)
            .await
            .unwrap();
        assert!(
            session
                .pending_backup
                .read()
                .as_ref()
                .unwrap()
                .snapshot
                .service_bundle
                .is_none()
        );
        assert!(
            session
                .pending_backup
                .read()
                .as_ref()
                .unwrap()
                .snapshot
                .payload
                .as_ref()
                .unwrap()
                .contains("enable: false")
        );
        let (generation, conflict) =
            session.accept_saved_service_tun(CommittedConfig::default(), prepared);
        assert_eq!(generation, 1);
        assert!(!conflict);
        let pending = session.pending_backup.read().clone().unwrap();
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(pending.snapshot.payload.as_ref().unwrap()).unwrap();
        assert_eq!(yaml["tun"]["enable"], true);
        assert!(
            pending
                .snapshot
                .service_bundle
                .unwrap()
                .yaml()
                .contains("assets/")
        );
        std::fs::remove_dir(&home).unwrap();
    }

    #[tokio::test]
    async fn later_pending_target_is_preserved_when_saved_publication_detects_conflict() {
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
        )
        .unwrap();
        let pending = PendingBackupRestore {
            snapshot: CoreRestoreSnapshot {
                committed: CommittedConfig::default(),
                payload: Some("mode: rule\n".into()),
                service_bundle: None,
            },
            store_root: PathBuf::from("pending-root"),
            generation: 0,
        };
        *session.pending_backup.write() = Some(pending.clone());
        session.pending_backup.write().as_mut().unwrap().generation = 1;
        session
            .pending_backup
            .write()
            .as_mut()
            .unwrap()
            .snapshot
            .payload = Some("mode: global\n".into());
        let (generation, conflict) =
            session.accept_saved_service_tun(CommittedConfig::default(), Some(pending));
        assert!(conflict);
        let current = session.pending_backup.read().clone().unwrap();
        assert_eq!(current.snapshot.payload.as_deref(), Some("mode: global\n"));
        assert_eq!(current.generation, generation);
    }
}
