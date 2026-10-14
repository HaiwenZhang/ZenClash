//! Catalog persistence participant and outcome interpretation for the admitted Service transaction.

use super::*;

enum CatalogOperation {
    ValidateCandidate,
    CommitCandidate,
}

/// One-shot catalog participant in the admitted service transaction.
pub(crate) struct ServiceProfileCommit {
    root: PathBuf,
    source_candidate: parking_lot::Mutex<Option<ProfileSourceCandidate>>,
    pub(super) committed: parking_lot::Mutex<Option<CommittedProfile>>,
    pub(super) failure: parking_lot::Mutex<Option<ProfileApplicationError>>,
    runtime_attempted: std::sync::atomic::AtomicBool,
    runtime_generation: parking_lot::Mutex<Option<u64>>,
}

impl ServiceProfileCommit {
    pub(super) fn new(source_candidate: ProfileSourceCandidate) -> Self {
        Self {
            root: source_candidate.store().root().to_path_buf(),
            source_candidate: parking_lot::Mutex::new(Some(source_candidate)),
            committed: parking_lot::Mutex::new(None),
            failure: parking_lot::Mutex::new(None),
            runtime_attempted: std::sync::atomic::AtomicBool::new(false),
            runtime_generation: parking_lot::Mutex::new(None),
        }
    }

    pub(crate) fn mark_runtime_attempted(&self) {
        self.runtime_attempted.store(true, Ordering::Release);
    }

    pub(crate) fn record_runtime_generation(&self, generation: u64) {
        if self.runtime_attempted.load(Ordering::Acquire) {
            *self.runtime_generation.lock() = Some(generation);
        }
    }

    pub(super) fn unsaved_outcome(
        &self,
        session: &CoreSession,
        recovery: ProfileRecovery,
        cause: ProfileApplicationError,
        restored: bool,
    ) -> ProfileApplyOutcome {
        if !self.runtime_attempted.load(Ordering::Acquire) {
            return ProfileApplyOutcome::Rejected {
                last_known_good: recovery.last_known_good,
                cause,
            };
        }
        let generation = self
            .runtime_generation
            .lock()
            .unwrap_or_else(|| session.invalidate_runtime_observations());
        if restored {
            ProfileApplyOutcome::RolledBack {
                last_known_good: recovery.last_known_good,
                cause,
                runtime_version: generation,
            }
        } else {
            ProfileApplyOutcome::RuntimeUnknown {
                recovery,
                cause,
                runtime_version: generation,
            }
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) async fn validate(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        self.perform_catalog_operation(lease, CatalogOperation::ValidateCandidate)
            .await
    }

    pub(crate) async fn commit(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
    ) -> Result<(), CoreSessionError> {
        self.perform_catalog_operation(lease, CatalogOperation::CommitCandidate)
            .await
    }

    async fn perform_catalog_operation(
        self: &std::sync::Arc<Self>,
        lease: &crate::data_coordinator::DataWriteLease,
        operation: CatalogOperation,
    ) -> Result<(), CoreSessionError> {
        if !lease.covers(&self.root) {
            return Err(MihomoError::StaleBinding.into());
        }
        let participant = self.clone();
        let lease = crate::data_coordinator::DataWriteAccess::new(&self.root)
            .authorized(lease)
            .borrowed_authority(std::slice::from_ref(&self.root))
            .map_err(crate::ControlledConfigError::Transaction)?
            .ok_or(MihomoError::StaleBinding)?;
        let result = run_store(move || {
            let mut pending = participant.source_candidate.lock();
            let source_candidate = pending.as_mut().ok_or_else(|| {
                ProfileStoreError::Transaction(zenclash_i18n::text("core_page.service.unknown"))
            })?;
            source_candidate.validate_source_current()?;
            if matches!(operation, CatalogOperation::CommitCandidate) {
                // Materialize only after the runtime trial, under the shared transaction lease.
                source_candidate
                    .materialize_for_commit(source_candidate.store().with_write_lease(&lease))?;
                let source_candidate = pending.take().ok_or_else(|| {
                    ProfileStoreError::Transaction(zenclash_i18n::text("core_page.service.unknown"))
                })?;
                *participant.committed.lock() = Some(source_candidate.commit()?);
            }
            Ok(())
        })
        .await;
        result.map_err(|error| {
            let message = error.to_string();
            *self.failure.lock() = Some(error);
            crate::ControlledConfigError::Transaction(message).into()
        })
    }
}

impl ProfileApplication {
    /// Applies a managed change, authorizing Service ownership for an effective TUN candidate.
    /// Returns capture facts separately from the directory receipt. Once admitted,
    /// completion retains both receipts even when its waiter stops waiting.
    pub async fn apply_with_service(
        &self,
        manager: &crate::ServiceManager,
        change: ProfileChange,
    ) -> (ProfileApplyOutcome, Option<crate::ServiceTunOutcome>) {
        let prepared = match self.prepare_change(change).await {
            Ok(prepared) => prepared,
            Err(error) => return (error.into(), None),
        };
        if !prepared.requires_service() {
            return (self.apply_prepared(prepared).await, None);
        }
        let last_known_good = prepared.source_candidate.last_known_good();
        let source_metadata = ProfileSourceMetadata::from(prepared.source_candidate.record());
        let recovery = ProfileRecovery {
            attempted: source_metadata.clone(),
            last_known_good: last_known_good.clone(),
        };
        if !manager.owns_profile_application(&self.session, &self.controlled) {
            return (
                ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: MihomoError::StaleBinding.into(),
                },
                None,
            );
        }
        let Some(crate::core_session::service_tun::PreparedConfig::Service(runtime)) =
            prepared.runtime
        else {
            return (
                ProfileApplyOutcome::Rejected {
                    last_known_good,
                    cause: MihomoError::StaleBinding.into(),
                },
                None,
            );
        };
        let participant = std::sync::Arc::new(ServiceProfileCommit::new(prepared.source_candidate));
        let mut runtime = (*runtime).clone();
        runtime.profile_commit = Some(participant.clone());
        let failure_recovery = recovery.clone();
        let application = self.clone();
        let manager = manager.clone();
        let completion = tokio::spawn(async move {
            let result = manager
                .apply_service_profile(std::sync::Arc::new(runtime))
                .await;
            let committed = participant.committed.lock().take();
            let failure = participant.failure.lock().take();
            let (service, mut manager_failure) = match result {
                Ok(service) => (Some(service), None),
                Err(error) => (
                    None,
                    Some(ProfileApplicationError::Service(Box::new(error))),
                ),
            };
            if let Some(committed) = committed {
                let core = service.as_ref().and_then(crate::ServiceTunOutcome::core);
                let pending = service
                    .as_ref()
                    .is_none_or(|service| service.commit_pending());
                let outcome = if let Some(core) = core.filter(|_| !pending) {
                    ProfileApplyOutcome::Applied {
                        profile: committed.record,
                        path: committed.path,
                        source_version: source_metadata,
                        runtime_version: core.generation,
                        kind: core.kind,
                    }
                } else {
                    ProfileApplyOutcome::CommittedButRuntimeUnknown {
                        profile: committed.record,
                        path: committed.path,
                        source_version: source_metadata,
                        cause: manager_failure.take().unwrap_or_else(|| {
                            ProfileApplicationError::Controller(MihomoError::Process(
                                zenclash_i18n::text("core_page.service.pending"),
                            ))
                        }),
                        runtime_version: core.map_or_else(
                            || {
                                participant.runtime_generation.lock().unwrap_or_else(|| {
                                    application.session.invalidate_runtime_observations()
                                })
                            },
                            |core| core.generation,
                        ),
                    }
                };
                return (outcome, service);
            }
            let restored = service.as_ref().is_some_and(|service| {
                matches!(service.capture(), crate::CaptureOutcome::RolledBack { .. })
            });
            let cause = failure.or(manager_failure).unwrap_or_else(|| {
                ProfileApplicationError::Controller(MihomoError::Process(
                    match service.as_ref().map(crate::ServiceTunOutcome::capture) {
                        Some(
                            crate::CaptureOutcome::RolledBack { failure, .. }
                            | crate::CaptureOutcome::ReconcileNeeded { failure, .. },
                        ) => failure.clone(),
                        _ => zenclash_i18n::text("core_page.service.unknown"),
                    },
                ))
            });
            let outcome =
                participant.unsaved_outcome(&application.session, recovery, cause, restored);
            (outcome, service)
        });
        completion.await.unwrap_or_else(|error| {
            (
                ProfileApplyOutcome::RuntimeUnknown {
                    recovery: failure_recovery,
                    cause: ProfileApplicationError::Task(error.to_string()),
                    runtime_version: self.session.invalidate_runtime_observations(),
                },
                None,
            )
        })
    }
}
