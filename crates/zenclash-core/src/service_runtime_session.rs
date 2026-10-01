//! Shared ownership of immutable service configuration revisions.

use crate::{MihomoError, MihomoResult, service_runtime::ServiceRuntimeBundle};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zenclash_service::ServiceClient;

pub(crate) trait RuntimeTransport: Send + Sync + 'static {
    fn stage(
        &self,
        bundle: &ServiceRuntimeBundle,
    ) -> impl Future<Output = MihomoResult<u64>> + Send;
    fn validate(&self, revision: u64) -> impl Future<Output = MihomoResult<()>> + Send;
    fn reload(&self, revision: u64, force: bool) -> impl Future<Output = MihomoResult<()>> + Send;
    fn commit(&self, revision: u64) -> impl Future<Output = MihomoResult<()>> + Send;
    fn revisions(&self) -> impl Future<Output = MihomoResult<(Option<u64>, Option<u64>)>> + Send;
    fn stop(&self) -> impl Future<Output = MihomoResult<()>> + Send;
    fn status(
        &self,
    ) -> impl Future<Output = MihomoResult<zenclash_service::ServiceRuntimeStatus>> + Send;
    fn start(&self, revision: u64) -> impl Future<Output = MihomoResult<()>> + Send;
    fn release(&self) -> impl Future<Output = MihomoResult<()>> + Send;
}

impl RuntimeTransport for ServiceClient {
    async fn stage(&self, bundle: &ServiceRuntimeBundle) -> MihomoResult<u64> {
        bundle.stage(self).await
    }
    async fn validate(&self, revision: u64) -> MihomoResult<()> {
        Ok(self.validate(revision).await?)
    }
    async fn reload(&self, revision: u64, force: bool) -> MihomoResult<()> {
        Ok(self.reload(revision, force).await?)
    }
    async fn commit(&self, revision: u64) -> MihomoResult<()> {
        Ok(self.commit_runtime(revision).await?)
    }
    async fn revisions(&self) -> MihomoResult<(Option<u64>, Option<u64>)> {
        let status = self.status().await?;
        Ok((status.applied_revision, status.committed_revision))
    }
    async fn stop(&self) -> MihomoResult<()> {
        Ok(self.stop().await?)
    }
    async fn status(&self) -> MihomoResult<zenclash_service::ServiceRuntimeStatus> {
        Ok(self.status().await?)
    }
    async fn start(&self, revision: u64) -> MihomoResult<()> {
        Ok(self.start(revision).await?)
    }
    async fn release(&self) -> MihomoResult<()> {
        Ok(self.release().await?)
    }
}

pub(crate) type ServiceRuntimeSession = RuntimeSession<ServiceClient>;

pub(crate) struct RuntimeSession<T: RuntimeTransport> {
    pub(crate) client: Arc<T>,
    source_home: PathBuf,
    state: Arc<Mutex<RuntimeState>>,
}

#[derive(Clone)]
struct RuntimeRevision {
    revision: u64,
    // Retain the exact resources until a successful commit retires this revision.
    _bundle: Arc<ServiceRuntimeBundle>,
}

#[derive(Default)]
struct RuntimeState {
    active: Option<RuntimeRevision>,
    candidate: Option<(RuntimeRevision, CandidatePhase)>,
    closing: bool,
    released: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidatePhase {
    Validated,
    Applied,
    Finalizing,
    Uncertain,
}

pub(crate) struct PreparedRuntime<T: RuntimeTransport = ServiceClient> {
    owner: Arc<RuntimeSession<T>>,
    state: OwnedMutexGuard<RuntimeState>,
    recovery_bundle: Option<Arc<ServiceRuntimeBundle>>,
}

pub(crate) struct AppliedRuntime<T: RuntimeTransport = ServiceClient> {
    owner: Arc<RuntimeSession<T>>,
    state: OwnedMutexGuard<RuntimeState>,
}

impl<T: RuntimeTransport> RuntimeSession<T> {
    pub(crate) fn new(client: Arc<T>, source_home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            client,
            source_home,
            state: Arc::new(Mutex::new(RuntimeState::default())),
        })
    }

    pub(crate) fn source_home(&self) -> &Path {
        &self.source_home
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        payload: &str,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        if state.candidate.is_some() {
            return Err(unknown());
        }
        // One user-authority read supplies both validation and later application.
        let bundle = Arc::new(
            ServiceRuntimeBundle::prepare(payload, self.source_home().to_path_buf()).await?,
        );
        self.prepare_locked(bundle, state).await
    }

    pub(crate) async fn prepare_recovery(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let state = self.state.clone().lock_owned().await;
        if state.closing { return Err(unknown()); }
        // A saved transaction can only be finalized; restoring its old snapshot would undo durable data.
        if state
            .candidate
            .as_ref()
            .is_some_and(|(_, phase)| *phase == CandidatePhase::Finalizing)
        {
            return Err(unknown());
        }
        Ok(PreparedRuntime {
            owner: self.clone(),
            state,
            recovery_bundle: Some(bundle),
        })
    }

    pub(crate) async fn prepare_restore(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
        authority: &crate::core_session::RuntimeRestoreAuthority,
    ) -> MihomoResult<PreparedRuntime<T>> {
        match authority {
            crate::core_session::RuntimeRestoreAuthority::Ordinary => {
                self.prepare_recovery(bundle).await
            }
            crate::core_session::RuntimeRestoreAuthority::Backup(admission) => {
                let _admission = admission.clone();
                self.prepare_backup_recovery(bundle).await
            }
        }
    }

    pub(crate) async fn prepare_backup_recovery(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        // Only an explicit whole-data backup restore may replace a durable saved revision.
        // Confirm it first; Stage must never erase an unconfirmed candidate.
        if state
            .candidate
            .as_ref()
            .is_some_and(|(_, phase)| *phase == CandidatePhase::Finalizing)
        {
            self.reconcile_locked(&mut state).await?;
        }
        drop(state);
        // Recheck the ordinary recovery boundary after confirmation. A concurrent durable
        // transaction may never be reverted merely because this backup confirmed an older one.
        self.prepare_recovery(bundle).await
    }

    async fn prepare_locked(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
        mut state: OwnedMutexGuard<RuntimeState>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let revision = self.client.stage(&bundle).await?;
        self.client.validate(revision).await?;
        state.candidate = Some((
            RuntimeRevision {
                revision,
                _bundle: bundle,
            },
            CandidatePhase::Validated,
        ));
        Ok(PreparedRuntime {
            owner: self.clone(),
            state,
            recovery_bundle: None,
        })
    }

    pub(crate) fn snapshot(&self) -> MihomoResult<Arc<ServiceRuntimeBundle>> {
        let state = self.state.try_lock().map_err(|_| unknown())?;
        if state.closing || state.candidate.is_some() {
            return Err(unknown());
        }
        state
            .active
            .as_ref()
            .map(|active| active._bundle.clone())
            .ok_or_else(|| MihomoError::Process("No accepted service runtime snapshot".into()))
    }

    pub(crate) async fn reconcile(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        self.reconcile_locked(&mut state).await
    }

    pub(crate) async fn stop_confirmed(&self) -> MihomoResult<()> {
        let state = self.state.lock().await;
        if state.closing {
            return Err(unknown());
        }
        self.confirm_stop().await
    }

    async fn confirm_stop(&self) -> MihomoResult<()> {
        let stopped = self.client.stop().await;
        // Even a lost Stop acknowledgement is definitive only after native readback.
        let status = self.client.status().await?;
        if !status.running && status.pid.is_none() {
            Ok(())
        } else {
            stopped.and(Err(unknown()))
        }
    }

    pub(crate) async fn restart_accepted(
        &self,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        self.reconcile_locked(&mut state).await?;
        let active = state.active.clone().ok_or_else(unknown)?;
        self.confirm_stop().await?;
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(unknown());
        }
        let started = self.client.start(active.revision).await;
        let status = self.client.status().await?;
        if status.running
            && status.pid.is_some()
            && status.applied_revision == Some(active.revision)
            && status.committed_revision == Some(active.revision)
        {
            Ok(())
        } else {
            started.and(Err(unknown()))
        }
    }

    pub(crate) async fn release_owned(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.released {
            return Ok(());
        }
        state.closing = true;
        // Release independently performs confirmed native Stop and closes heartbeat admission.
        // Attempt it even if Status is unavailable; failure retains the immutable diagnostic snapshot.
        let _stop = self.confirm_stop().await;
        self.client.release().await?;
        state.active = None;
        state.candidate = None;
        state.released = true;
        Ok(())
    }

    pub(crate) async fn restore_active(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing { return Err(unknown()); }
        if state
            .candidate
            .as_ref()
            .is_some_and(|(_, phase)| *phase == CandidatePhase::Finalizing)
        {
            return Err(unknown());
        }
        if let Some(active) = &state.active {
            self.client.reload(active.revision, true).await?;
            state.candidate = None;
            Ok(())
        } else {
            self.client.stop().await?;
            state.candidate = None;
            Err(MihomoError::Process(
                "No accepted service runtime snapshot; owned kernel stopped".into(),
            ))
        }
    }

    async fn reconcile_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        if state.closing {
            return Err(unknown());
        }
        let Some((candidate, phase)) = state.candidate.as_ref() else {
            return Ok(());
        };
        match phase {
            CandidatePhase::Validated => {
                state.candidate = None;
                Ok(())
            }
            CandidatePhase::Finalizing => {
                let revision = candidate.revision;
                let (applied, committed) = self.client.revisions().await?;
                if applied != Some(revision) {
                    return Err(unknown());
                }
                if committed != Some(revision) {
                    self.client.commit(revision).await?;
                }
                state.active = state.candidate.take().map(|(revision, _)| revision);
                Ok(())
            }
            CandidatePhase::Applied | CandidatePhase::Uncertain => Err(unknown()),
        }
    }

    pub(crate) async fn adopt(
        &self,
        bundle: ServiceRuntimeBundle,
        revision: u64,
    ) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing || state.candidate.is_some() {
            return Err(unknown());
        }
        let (applied, committed) = self.client.revisions().await?;
        if applied != Some(revision) || committed != Some(revision) {
            return Err(unknown());
        }
        state.active = Some(RuntimeRevision {
            revision,
            _bundle: Arc::new(bundle),
        });
        Ok(())
    }
}

impl<T: RuntimeTransport> PreparedRuntime<T> {
    pub(crate) async fn apply(mut self, force: bool) -> MihomoResult<AppliedRuntime<T>> {
        if let Some(bundle) = self.recovery_bundle.take() {
            let Some(active) = self.state.active.clone() else {
                self.owner.client.stop().await?;
                self.state.candidate = None;
                return Err(MihomoError::Process(
                    "No accepted service runtime snapshot; owned kernel stopped".into(),
                ));
            };
            // Recover the validated active revision before Stage can retire an unsaved candidate.
            self.state.candidate = Some((active.clone(), CandidatePhase::Uncertain));
            self.owner.client.reload(active.revision, true).await?;
            if Arc::ptr_eq(&active._bundle, &bundle) {
                self.state.candidate = Some((active, CandidatePhase::Applied));
                return Ok(AppliedRuntime {
                    owner: self.owner,
                    state: self.state,
                });
            }
            self.state.candidate = None;
            let revision = self.owner.client.stage(&bundle).await?;
            self.owner.client.validate(revision).await?;
            self.state.candidate = Some((
                RuntimeRevision {
                    revision,
                    _bundle: bundle,
                },
                CandidatePhase::Validated,
            ));
        }
        let Some((candidate, phase)) = self.state.candidate.as_mut() else {
            return Err(unknown());
        };
        // Cancellation or lost response must not permit a new Stage to erase recovery resources.
        *phase = CandidatePhase::Uncertain;
        if let Err(error) = self.owner.client.reload(candidate.revision, force).await {
            if !error.mutation_result_unknown() {
                self.state.candidate = None;
            }
            return Err(error);
        }
        if let Some((_, phase)) = self.state.candidate.as_mut() {
            *phase = CandidatePhase::Applied;
        }
        Ok(AppliedRuntime {
            owner: self.owner,
            state: self.state,
        })
    }
}

impl<T: RuntimeTransport> AppliedRuntime<T> {
    pub(crate) async fn commit(mut self) -> MihomoResult<()> {
        let Some((candidate, phase)) = self.state.candidate.as_mut() else {
            return Err(unknown());
        };
        *phase = CandidatePhase::Finalizing;
        self.owner.client.commit(candidate.revision).await?;
        self.state.active = self.state.candidate.take().map(|(revision, _)| revision);
        Ok(())
    }

    pub(crate) async fn rollback(mut self) -> MihomoResult<()> {
        if let Some((_, phase)) = self.state.candidate.as_mut() {
            *phase = CandidatePhase::Uncertain;
        }
        if let Some(active) = &self.state.active {
            self.owner.client.reload(active.revision, true).await?;
            self.state.candidate = None;
            Ok(())
        } else {
            self.owner.client.stop().await?;
            self.state.candidate = None;
            Err(MihomoError::Process(
                "No accepted service runtime snapshot; owned kernel stopped".into(),
            ))
        }
    }
}

fn unknown() -> MihomoError {
    MihomoError::Service(zenclash_service::ServiceClientError::Rejected(
        zenclash_service::ServiceErrorCode::OutcomeUnknown,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    #[derive(Default)]
    struct Service {
        next: AtomicU64,
        applied: AtomicU64,
        committed: AtomicU64,
        lose_commit_ack: AtomicBool,
        lose_commit_before_apply: AtomicBool,
        lose_reload_ack: AtomicBool,
        lose_status_ack: AtomicBool,
        calls: Mutex<Vec<String>>,
        running: AtomicBool,
        lose_stop_ack: AtomicBool,
        stop_has_no_effect: AtomicBool,
        lose_start_ack: AtomicBool,
    }

    impl RuntimeTransport for Service {
        async fn stage(&self, bundle: &ServiceRuntimeBundle) -> MihomoResult<u64> {
            let revision = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            self.calls
                .lock()
                .push(format!("stage:{revision}:{}", bundle.yaml()));
            Ok(revision)
        }
        async fn validate(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("validate:{revision}"));
            Ok(())
        }
        async fn reload(&self, revision: u64, _force: bool) -> MihomoResult<()> {
            self.calls.lock().push(format!("reload:{revision}"));
            self.applied.store(revision, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
            if self.lose_reload_ack.swap(false, Ordering::SeqCst) {
                return Err(unknown());
            }
            Ok(())
        }
        async fn commit(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("commit:{revision}"));
            if self.lose_commit_before_apply.swap(false, Ordering::SeqCst) {
                return Err(unknown());
            }
            self.committed.store(revision, Ordering::SeqCst);
            if self.lose_commit_ack.swap(false, Ordering::SeqCst) {
                return Err(MihomoError::Service(
                    zenclash_service::ServiceClientError::Rejected(
                        zenclash_service::ServiceErrorCode::OutcomeUnknown,
                    ),
                ));
            }
            Ok(())
        }
        async fn revisions(&self) -> MihomoResult<(Option<u64>, Option<u64>)> {
            self.calls.lock().push("status".into());
            if self.lose_status_ack.swap(false, Ordering::SeqCst) {
                return Err(unknown());
            }
            let optional = |value| if value == 0 { None } else { Some(value) };
            Ok((
                optional(self.applied.load(Ordering::SeqCst)),
                optional(self.committed.load(Ordering::SeqCst)),
            ))
        }
        async fn stop(&self) -> MihomoResult<()> {
            self.calls.lock().push("stop".into());
            if !self.stop_has_no_effect.load(Ordering::SeqCst) {
                self.applied.store(0, Ordering::SeqCst);
                self.running.store(false, Ordering::SeqCst);
            }
            if self.lose_stop_ack.swap(false, Ordering::SeqCst) {
                Err(unknown())
            } else {
                Ok(())
            }
        }
        async fn status(&self) -> MihomoResult<zenclash_service::ServiceRuntimeStatus> {
            let (applied_revision, committed_revision) = self.revisions().await?;
            let running = self.running.load(Ordering::SeqCst);
            Ok(zenclash_service::ServiceRuntimeStatus {
                applied_revision,
                committed_revision,
                running,
                pid: running.then_some(123),
                exit_reason: None,
                candidate: None,
            })
        }
        async fn start(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("start:{revision}"));
            assert!(!self.running.swap(true, Ordering::SeqCst), "double start");
            self.applied.store(revision, Ordering::SeqCst);
            if self.lose_start_ack.swap(false, Ordering::SeqCst) {
                Err(unknown())
            } else {
                Ok(())
            }
        }
        async fn release(&self) -> MihomoResult<()> {
            self.calls.lock().push("release".into());
            self.stop().await
        }
    }

    fn home() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "zenclash-runtime-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    // Protocol-state fixture only: these assertions do not prove native service execution.
    async fn accepted_owner() -> (Arc<Service>, Arc<RuntimeSession<Service>>) {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        (service, owner)
    }

    #[tokio::test]
    async fn lifecycle_restarts_accepted_revision_after_newer_candidate_validation() {
        let (service, owner) = accepted_owner().await;
        let newer = owner.prepare("mode: global\n").await.unwrap();
        drop(newer);
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "start:1", "status"]
        );
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
    }

    #[tokio::test]
    async fn lost_stop_ack_requires_stopped_status_before_start() {
        let (service, owner) = accepted_owner().await;
        service.lose_stop_ack.store(true, Ordering::SeqCst);
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "start:1", "status"]
        );
    }

    #[tokio::test]
    async fn unknown_live_stop_never_starts_a_second_kernel() {
        let (service, owner) = accepted_owner().await;
        service.lose_stop_ack.store(true, Ordering::SeqCst);
        service.stop_has_no_effect.store(true, Ordering::SeqCst);
        service.calls.lock().clear();
        assert!(
            owner
                .restart_accepted(&AtomicBool::new(false))
                .await
                .is_err()
        );
        assert_eq!(*service.calls.lock(), ["stop", "status"]);
        assert!(service.running.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn lost_start_ack_is_confirmed_without_a_second_start() {
        let (service, owner) = accepted_owner().await;
        service.lose_start_ack.store(true, Ordering::SeqCst);
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "start:1", "status"]
        );
    }

    #[tokio::test]
    async fn shutdown_cancellation_after_stop_blocks_start_and_retains_exact_snapshot() {
        let (service, owner) = accepted_owner().await;
        let snapshot = owner.snapshot().unwrap();
        service.calls.lock().clear();
        assert!(
            owner
                .restart_accepted(&AtomicBool::new(true))
                .await
                .is_err()
        );
        assert_eq!(*service.calls.lock(), ["stop", "status"]);
        assert!(Arc::ptr_eq(&snapshot, &owner.snapshot().unwrap()));
        owner.release_owned().await.unwrap();
        assert!(!service.running.load(Ordering::SeqCst));
        assert!(
            owner
                .restart_accepted(&AtomicBool::new(false))
                .await
                .is_err()
        );
        assert!(owner.prepare("mode: direct\n").await.is_err());
        owner.release_owned().await.unwrap();
    }

    #[tokio::test]
    async fn validated_candidate_does_not_reread_deleted_resources_and_rollback_uses_old_revision()
    {
        let home = home();
        std::fs::create_dir_all(&home).unwrap();
        let asset = home.join("provider.yaml");
        std::fs::write(&asset, "payload: [DOMAIN,example.com]\n").unwrap();
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home.clone());
        let first = "mode: rule\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n";
        owner
            .prepare(first)
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        std::fs::write(&asset, "payload: [DOMAIN,new.example.com]\n").unwrap();
        let candidate = owner.prepare("mode: global\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n").await.unwrap();
        std::fs::remove_file(asset).unwrap();
        let applied = candidate.apply(true).await.unwrap();
        applied.rollback().await.unwrap();
        let calls = service.calls.lock();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("stage:"))
                .count(),
            2
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("validate:"))
                .count(),
            2
        );
        assert_eq!(calls.last().unwrap(), "reload:1");
        drop(calls);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn lost_commit_ack_preserves_candidate_and_status_finalizes_without_reloading() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        let applied = owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        service.lose_commit_ack.store(true, Ordering::SeqCst);
        assert!(
            applied
                .commit()
                .await
                .unwrap_err()
                .mutation_result_unknown()
        );
        owner.reconcile().await.unwrap();
        assert_eq!(
            owner.state.lock().await.active.as_ref().unwrap().revision,
            1
        );
        assert_eq!(
            service
                .calls
                .lock()
                .iter()
                .filter(|call| call.starts_with("reload:"))
                .count(),
            1
        );
        assert!(owner.state.lock().await.candidate.is_none());
    }

    #[tokio::test]
    async fn pending_finalize_retries_only_commit_and_rejects_rollback_of_saved_data() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        let applied = owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        service
            .lose_commit_before_apply
            .store(true, Ordering::SeqCst);
        assert!(applied.commit().await.is_err());
        assert!(owner.restore_active().await.is_err());
        owner.reconcile().await.unwrap();
        let calls = service.calls.lock();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("stage:"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("reload:"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("commit:"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn dropped_unsaved_candidate_blocks_new_stage_until_explicit_revision_recovery() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let candidate = owner
            .prepare("mode: global\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        drop(candidate);
        assert!(owner.prepare("mode: direct\n").await.is_err());
        owner.restore_active().await.unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        assert_eq!(service.next.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn snapshot_recovery_cannot_revert_a_saved_pending_finalize() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let snapshot = owner.snapshot().unwrap();
        let candidate = owner
            .prepare("mode: global\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        service
            .lose_commit_before_apply
            .store(true, Ordering::SeqCst);
        assert!(candidate.commit().await.is_err());
        assert!(owner.prepare_recovery(snapshot).await.is_err());
        assert_eq!(service.applied.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn production_restore_route_requires_backup_admission_to_replace_finalizing() {
        use crate::core_session::RuntimeRestoreAuthority;
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let snapshot = owner.snapshot().unwrap();
        let candidate = owner
            .prepare("mode: global\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        service
            .lose_commit_before_apply
            .store(true, Ordering::SeqCst);
        assert!(candidate.commit().await.is_err());
        let calls = service.calls.lock().clone();
        let denied = owner
            .prepare_restore(snapshot.clone(), &RuntimeRestoreAuthority::Ordinary)
            .await;
        assert!(
            denied.is_err(),
            "ordinary client restore route confirmed a durable pending revision"
        );
        assert_eq!(
            *service.calls.lock(),
            calls,
            "ordinary restoration performed native mutations or confirmation"
        );
        let state = owner.state.lock().await;
        assert_eq!(state.active.as_ref().unwrap().revision, 1);
        assert!(
            matches!(state.candidate.as_ref(), Some((candidate, CandidatePhase::Finalizing)) if candidate.revision == 2)
        );
        drop(state);
        let session = crate::CoreSession::open(
            crate::CoreKind::Mihomo,
            crate::MihomoClient::new(crate::MihomoEndpoint::new("http://127.0.0.1:1", "")).unwrap(),
        )
        .unwrap();
        let admission = session.begin_backup_restore().await.unwrap();
        owner
            .prepare_restore(
                snapshot,
                &RuntimeRestoreAuthority::Backup(admission.clone()),
            )
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 3);
        assert_eq!(service.committed.load(Ordering::SeqCst), 3);
        {
            let calls = service.calls.lock();
            let status = calls.iter().position(|call| call == "status").unwrap();
            let confirmed = calls.iter().rposition(|call| call == "commit:2").unwrap();
            let restored = calls
                .iter()
                .position(|call| call.starts_with("stage:3:"))
                .unwrap();
            assert!(status < confirmed && confirmed < restored);
        }
        assert!(owner.state.lock().await.candidate.is_none());
        drop(admission);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn exact_snapshot_recovers_a_lost_reload_ack_after_provider_sources_are_deleted() {
        let home = home();
        std::fs::create_dir_all(&home).unwrap();
        let asset = home.join("provider.yaml");
        std::fs::write(&asset, "payload: [example.com]\n").unwrap();
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home.clone());
        let config = "mode: rule\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n";
        let source = home.join("source.yaml");
        std::fs::write(&source, config).unwrap();
        owner
            .prepare(&std::fs::read_to_string(&source).unwrap())
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let snapshot = owner.snapshot().unwrap();
        let candidate = owner.prepare("mode: global\n").await.unwrap();
        service.lose_reload_ack.store(true, Ordering::SeqCst);
        assert!(
            candidate
                .apply(true)
                .await
                .err()
                .unwrap()
                .mutation_result_unknown()
        );
        std::fs::remove_file(asset).unwrap();
        std::fs::remove_file(source).unwrap();
        owner
            .prepare_recovery(snapshot)
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        assert_eq!(service.committed.load(Ordering::SeqCst), 1);
        assert_eq!(
            service.next.load(Ordering::SeqCst),
            2,
            "recovery must not erase candidate resources with a new Stage"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn backup_snapshot_finalizes_lost_commit_before_restoring_held_resources() {
        let home = home();
        std::fs::create_dir_all(&home).unwrap();
        let asset = home.join("provider.yaml");
        let source = home.join("source.yaml");
        std::fs::write(&asset, "payload: [old.example.com]\n").unwrap();
        let config = "mode: rule\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n";
        std::fs::write(&source, config).unwrap();
        let imported = home.join("imported");
        crate::AppPreferencesStore::new(home.join("preferences.json"))
            .save(&crate::AppPreferences {
                appearance: crate::AppearancePreference::Dark,
                ..Default::default()
            })
            .unwrap();
        crate::AppPreferencesStore::new(imported.join("preferences.json"))
            .save(&crate::AppPreferences {
                appearance: crate::AppearancePreference::Light,
                ..Default::default()
            })
            .unwrap();
        let archive = home.join("backup.zip");
        crate::BackupManager::new(&imported)
            .export_to(&archive)
            .unwrap();
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home.clone());
        owner
            .prepare(config)
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let snapshot = owner.snapshot().unwrap();
        let transaction = crate::BackupManager::new(&home)
            .prepare_restore(&archive)
            .unwrap()
            .activate()
            .unwrap();
        let candidate = owner
            .prepare("mode: global\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        service.lose_commit_ack.store(true, Ordering::SeqCst);
        assert!(candidate.commit().await.is_err());
        // The enclosing backup has restored its old files, independently of the service's ack.
        transaction.rollback().unwrap();
        assert_eq!(
            crate::AppPreferencesStore::new(home.join("preferences.json"))
                .load()
                .unwrap()
                .appearance,
            crate::AppearancePreference::Dark
        );
        std::fs::remove_file(asset).unwrap();
        std::fs::remove_file(source).unwrap();
        assert!(owner.prepare_recovery(snapshot.clone()).await.is_err());
        service.lose_status_ack.store(true, Ordering::SeqCst);
        assert!(
            owner
                .prepare_backup_recovery(snapshot.clone())
                .await
                .is_err()
        );
        assert_eq!(
            service.next.load(Ordering::SeqCst),
            2,
            "confirmation failure must not Stage or discard held recovery resources"
        );
        owner
            .prepare_backup_recovery(snapshot)
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 3);
        assert_eq!(service.committed.load(Ordering::SeqCst), 3);
        let calls = service.calls.lock();
        let status = calls.iter().position(|call| call == "status").unwrap();
        let restored = calls
            .iter()
            .position(|call| call.starts_with("stage:3:"))
            .unwrap();
        assert!(status < restored);
        assert!(calls[restored].contains("mode: rule"));
        drop(calls);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn rollback_without_an_accepted_snapshot_stops_owned_kernel_and_reports_failure() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        let candidate = owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap();
        assert!(candidate.rollback().await.is_err());
        assert_eq!(service.applied.load(Ordering::SeqCst), 0);
    }
}
