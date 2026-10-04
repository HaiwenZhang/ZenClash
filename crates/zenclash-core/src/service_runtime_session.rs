//! Shared ownership of immutable service configuration revisions.

use crate::{MihomoError, MihomoResult, service_runtime::ServiceRuntimeBundle};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zenclash_service::ServiceClient;

mod local_recovery;

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
    fn prepare_patch(
        &self,
        base: u64,
        patch: &serde_json::Value,
    ) -> impl Future<Output = MihomoResult<zenclash_service::ServicePreparedRuntimePatch>> + Send;
    fn apply_patch(&self, revision: u64) -> impl Future<Output = MihomoResult<()>> + Send;
    fn restore_patch(&self, revision: u64) -> impl Future<Output = MihomoResult<()>> + Send;
    fn read_cache(
        &self,
        revision: u64,
        kind: zenclash_service::ProviderKind,
        name: &str,
    ) -> impl Future<Output = MihomoResult<Option<Vec<u8>>>> + Send;
}

impl RuntimeTransport for ServiceClient {
    async fn read_cache(
        &self,
        revision: u64,
        kind: zenclash_service::ProviderKind,
        name: &str,
    ) -> MihomoResult<Option<Vec<u8>>> {
        Ok(self
            .read_complete_provider_cache(revision, kind, name)
            .await?)
    }
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
    async fn prepare_patch(
        &self,
        base: u64,
        patch: &serde_json::Value,
    ) -> MihomoResult<zenclash_service::ServicePreparedRuntimePatch> {
        Ok(self.prepare_runtime_patch(base, patch).await?)
    }
    async fn apply_patch(&self, revision: u64) -> MihomoResult<()> {
        Ok(self.apply_runtime_patch(revision).await?)
    }
    async fn restore_patch(&self, revision: u64) -> MihomoResult<()> {
        Ok(self.restore_runtime_patch(revision).await?)
    }
}

pub(crate) type ServiceRuntimeSession = RuntimeSession<ServiceClient>;

pub(crate) struct RuntimeSession<T: RuntimeTransport> {
    pub(crate) client: Arc<T>,
    source_home: PathBuf,
    local_launch: Option<crate::MihomoLaunchConfig>,
    state: Arc<Mutex<RuntimeState>>,
}

#[derive(Clone)]
struct RuntimeRevision {
    revision: u64,
    kind: RevisionKind,
    // Retain the exact resources until a successful commit retires this revision.
    _bundle: Arc<ServiceRuntimeBundle>,
}

#[derive(Clone)]
enum RevisionKind {
    Full,
    Patch { base: u64, delta: serde_json::Value },
}

#[derive(Default)]
struct RuntimeState {
    active: Option<RuntimeRevision>,
    candidate: Option<(RuntimeRevision, CandidatePhase)>,
    pending_patch_base: Option<u64>,
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
            local_launch: None,
            state: Arc::new(Mutex::new(RuntimeState::default())),
        })
    }

    pub(crate) fn from_local(client: Arc<T>, launch: crate::MihomoLaunchConfig) -> Arc<Self> {
        Arc::new(Self {
            client,
            source_home: launch.home_dir.clone(),
            local_launch: Some(launch),
            state: Arc::new(Mutex::new(RuntimeState::default())),
        })
    }

    pub(crate) fn local_launch(&self) -> Option<&crate::MihomoLaunchConfig> {
        self.local_launch.as_ref()
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

    pub(crate) async fn prepare_bundle(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        if state.candidate.is_some() {
            return Err(unknown());
        }
        self.prepare_locked(bundle, state).await
    }

    pub(crate) async fn prepare_patch(
        self: &Arc<Self>,
        patch: &serde_json::Value,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        if state.candidate.is_some() {
            return Err(unknown());
        }
        let active = state
            .active
            .clone()
            .ok_or_else(|| MihomoError::Process("No accepted service runtime snapshot".into()))?;
        let base = active.revision;
        // Reserve the session before sending: lost preparation acknowledgements must
        // not allow Stage to erase an unaccounted native candidate.
        state.pending_patch_base = Some(base);
        let prepared = match self.client.prepare_patch(base, patch).await {
            Ok(prepared) => prepared,
            Err(error) => {
                if !error.mutation_result_unknown() {
                    state.pending_patch_base = None;
                }
                return Err(error);
            }
        };
        state.pending_patch_base = None;
        // No mutable source reads: preserve the exact accepted asset bytes.
        let previous_bundle = active._bundle.clone();
        state.candidate = Some((
            RuntimeRevision {
                revision: prepared.revision,
                kind: RevisionKind::Patch {
                    base,
                    delta: prepared.effective_patch,
                },
                _bundle: previous_bundle.clone(),
            },
            CandidatePhase::Validated,
        ));
        let delta = match &state.candidate.as_ref().ok_or_else(unknown)?.0.kind {
            RevisionKind::Patch { delta, .. } => delta,
            RevisionKind::Full => return Err(unknown()),
        };
        let bundle = Arc::new(previous_bundle.with_delta(delta)?);
        state.candidate.as_mut().ok_or_else(unknown)?.0._bundle = bundle;
        Ok(PreparedRuntime {
            owner: self.clone(),
            state,
            recovery_bundle: None,
        })
    }

    pub(crate) async fn prepare_recovery(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let state = self.state.clone().lock_owned().await;
        if state.closing {
            return Err(unknown());
        }
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
                kind: RevisionKind::Full,
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
        if state.closing || state.candidate.is_some() || state.pending_patch_base.is_some() {
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
        let mut state = self.state.lock().await;
        if state.closing {
            return Err(unknown());
        }
        self.confirm_finalizing_locked(&mut state).await?;
        self.confirm_stop().await
    }

    pub(crate) async fn stop_and_export(&self) -> MihomoResult<Arc<ServiceRuntimeBundle>> {
        let mut state = self.state.lock().await;
        if state.closing || state.released {
            return Err(unknown());
        }
        self.confirm_finalizing_locked(&mut state).await?;
        if state.candidate.is_some() || state.pending_patch_base.is_some() {
            return Err(unknown());
        }
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| MihomoError::Process("No accepted service runtime snapshot".into()))?;
        let providers = active._bundle.cache_providers()?;
        let revision = active.revision;
        let mut bundle = active._bundle.cache_export_base(&providers);
        self.confirm_stop().await?;
        let export = async {
            let status = self.client.status().await?;
            if !export_status_matches(&status, revision) {
                return Err(unknown());
            }
            for provider in providers {
                let bytes = self
                    .client
                    .read_cache(revision, provider.kind, &provider.name)
                    .await?;
                bundle.replace_provider_cache(&provider, bytes)?;
            }
            if !export_status_matches(&self.client.status().await?, revision) {
                return Err(unknown());
            }
            Ok::<_, MihomoError>(Arc::new(bundle))
        };
        let bundle = tokio::time::timeout(std::time::Duration::from_secs(15), export)
            .await
            .map_err(|_| unknown())??;
        let active = state.active.as_mut().ok_or_else(unknown)?;
        active._bundle = bundle.clone();
        Ok(bundle)
    }

    pub(crate) async fn confirm_finalizing_before_stop(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing {
            return Err(unknown());
        }
        self.confirm_finalizing_locked(&mut state).await
    }

    async fn confirm_finalizing_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        if matches!(state.candidate, Some((_, CandidatePhase::Finalizing))) {
            self.reconcile_locked(state).await?;
        }
        Ok(())
    }

    async fn confirm_stop(&self) -> MihomoResult<()> {
        let stopped = self.client.stop().await;
        // Even a lost Stop acknowledgement is definitive only after native readback.
        let status = match self.client.status().await {
            Ok(status) => status,
            Err(error) => {
                return Err(
                    if stopped.is_ok()
                        || stopped
                            .as_ref()
                            .is_err_and(|error| error.mutation_result_unknown())
                    {
                        unknown()
                    } else {
                        error
                    },
                );
            }
        };
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
        let active = state
            .active
            .clone()
            .ok_or_else(|| MihomoError::Process("No accepted service runtime snapshot".into()))?;
        self.confirm_stop().await?;
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(unknown());
        }
        let _started = self.client.start(active.revision).await;
        // Stop has already been confirmed; any later rejection cannot describe a
        // definitive no-effect maintenance operation, even if Start was rejected.
        let status = self.client.status().await.map_err(|_| unknown())?;
        if status.running
            && status.pid.is_some()
            && status.applied_revision == Some(active.revision)
            && status.committed_revision == Some(active.revision)
        {
            Ok(())
        } else {
            Err(unknown())
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
        state.pending_patch_base = None;
        state.released = true;
        Ok(())
    }

    pub(crate) async fn restore_active(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing {
            return Err(unknown());
        }
        if state
            .candidate
            .as_ref()
            .is_some_and(|(_, phase)| *phase == CandidatePhase::Finalizing)
        {
            return Err(unknown());
        }
        self.restore_unsaved_locked(&mut state).await
    }

    async fn restore_unsaved_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        let Some(active) = state.active.clone() else {
            self.client.stop().await?;
            state.candidate = None;
            return Err(MihomoError::Process(
                "No accepted service runtime snapshot; owned kernel stopped".into(),
            ));
        };
        let had_pending_preparation = state.pending_patch_base.is_some();
        self.reconcile_preparation(state).await?;
        if had_pending_preparation && state.candidate.is_none() {
            return Ok(());
        }
        if let Some((candidate, phase)) = state.candidate.as_mut()
            && let RevisionKind::Patch { base, .. } = candidate.kind
        {
            if base != active.revision {
                return Err(unknown());
            }
            let status = self.client.status().await?;
            if !status.running && status.pid.is_none() {
                self.restore_stopped_patch(candidate.revision, base, &status)
                    .await?;
                state.candidate = None;
                return Ok(());
            }
            if !status.running || status.pid.is_none() {
                return Err(unknown());
            }
            if status.candidate.is_none()
                && status.applied_revision == Some(base)
                && status.committed_revision == Some(base)
            {
                state.candidate = None;
                return Ok(());
            }
            *phase = CandidatePhase::Uncertain;
            self.client.restore_patch(candidate.revision).await?;
            self.client.commit(base).await?;
        } else {
            let mut recovery = active.clone();
            recovery.kind = RevisionKind::Full;
            state.candidate = Some((recovery, CandidatePhase::Uncertain));
            self.client.reload(active.revision, true).await?;
        }
        state.candidate = None;
        Ok(())
    }

    async fn reconcile_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        if state.closing {
            return Err(unknown());
        }
        self.reconcile_preparation(state).await?;
        let Some((candidate, phase)) = state.candidate.as_ref() else {
            return Ok(());
        };
        match phase {
            CandidatePhase::Validated => {
                if matches!(candidate.kind, RevisionKind::Patch { .. }) {
                    return self.restore_unsaved_locked(state).await;
                }
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

    async fn reconcile_preparation(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        let Some(base) = state.pending_patch_base else {
            return Ok(());
        };
        let status = self.client.status().await?;
        if state.active.as_ref().map(|active| active.revision) != Some(base) {
            return Err(unknown());
        }
        if !status.running && status.pid.is_none() {
            if let Some(candidate) = &status.candidate {
                if candidate.phase != zenclash_service::ServiceRuntimeCandidatePhase::Prepared {
                    return Err(unknown());
                }
                self.restore_stopped_patch(candidate.revision, base, &status)
                    .await?;
            } else if status.committed_revision != Some(base) || status.applied_revision.is_some() {
                return Err(unknown());
            }
            state.pending_patch_base = None;
            return Ok(());
        }
        if !status.running
            || status.pid.is_none()
            || status.applied_revision != Some(base)
            || status.committed_revision != Some(base)
        {
            return Err(unknown());
        }
        if let Some(candidate) = status.candidate {
            if candidate.kind != zenclash_service::ServiceRuntimeCandidateKind::Patch
                || candidate.base_revision != Some(base)
                || candidate.phase != zenclash_service::ServiceRuntimeCandidatePhase::Prepared
            {
                return Err(unknown());
            }
            self.client.restore_patch(candidate.revision).await?;
            self.client.commit(base).await?;
        }
        state.pending_patch_base = None;
        Ok(())
    }

    async fn restore_stopped_patch(
        &self,
        revision: u64,
        base: u64,
        status: &zenclash_service::ServiceRuntimeStatus,
    ) -> MihomoResult<()> {
        if status.running
            || status.pid.is_some()
            || status.applied_revision.is_some()
            || status.committed_revision != Some(base)
        {
            return Err(unknown());
        }
        if let Some(candidate) = &status.candidate {
            if candidate.revision != revision
                || candidate.kind != zenclash_service::ServiceRuntimeCandidateKind::Patch
                || candidate.base_revision != Some(base)
            {
                return Err(unknown());
            }
            let restored = self.client.restore_patch(revision).await;
            let fresh = self.client.status().await?;
            if fresh.running
                || fresh.pid.is_some()
                || fresh.applied_revision.is_some()
                || fresh.committed_revision != Some(base)
                || fresh.candidate.is_some()
            {
                return Err(restored.err().unwrap_or_else(unknown));
            }
        }
        Ok(())
    }

    pub(crate) async fn adopt(
        &self,
        bundle: ServiceRuntimeBundle,
        revision: u64,
    ) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing || state.candidate.is_some() || state.pending_patch_base.is_some() {
            return Err(unknown());
        }
        let (applied, committed) = self.client.revisions().await?;
        if applied != Some(revision) || committed != Some(revision) {
            return Err(unknown());
        }
        state.active = Some(RuntimeRevision {
            revision,
            kind: RevisionKind::Full,
            _bundle: Arc::new(bundle),
        });
        Ok(())
    }
}

impl<T: RuntimeTransport> PreparedRuntime<T> {
    pub(crate) fn effective_delta(&self) -> Option<&serde_json::Value> {
        match &self.state.candidate.as_ref()?.0.kind {
            RevisionKind::Full => None,
            RevisionKind::Patch { delta, .. } => Some(delta),
        }
    }
    pub(crate) async fn apply(mut self, force: bool) -> MihomoResult<AppliedRuntime<T>> {
        if let Some(bundle) = self.recovery_bundle.take() {
            let Some(active) = self.state.active.clone() else {
                self.owner.client.stop().await?;
                self.state.candidate = None;
                return Err(MihomoError::Process(
                    "No accepted service runtime snapshot; owned kernel stopped".into(),
                ));
            };
            // Restore the exact unsaved candidate before Stage can retire resources.
            self.owner.restore_unsaved_locked(&mut self.state).await?;
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
                    kind: RevisionKind::Full,
                    _bundle: bundle,
                },
                CandidatePhase::Validated,
            ));
        }
        let initial = self.state.active.is_none();
        let Some((candidate, phase)) = self.state.candidate.as_mut() else {
            return Err(unknown());
        };
        // Cancellation or lost response must not permit a new Stage to erase recovery resources.
        *phase = CandidatePhase::Uncertain;
        let applied = match &candidate.kind {
            RevisionKind::Full if initial => {
                let started = self.owner.client.start(candidate.revision).await;
                match self.owner.client.status().await {
                    Ok(status)
                        if status.running
                            && status.pid.is_some()
                            && status.applied_revision == Some(candidate.revision) =>
                    {
                        Ok(())
                    }
                    Ok(status)
                        if !status.running
                            && status.pid.is_none()
                            && started
                                .as_ref()
                                .is_err_and(|error| !error.mutation_result_unknown()) =>
                    {
                        started
                    }
                    _ => Err(unknown()),
                }
            }
            RevisionKind::Full => self.owner.client.reload(candidate.revision, force).await,
            RevisionKind::Patch { .. } => self.owner.client.apply_patch(candidate.revision).await,
        };
        if let Err(error) = applied {
            if !error.mutation_result_unknown() {
                if matches!(candidate.kind, RevisionKind::Patch { .. }) {
                    // A definite application rejection retains the prepared server candidate.
                    *phase = CandidatePhase::Validated;
                } else {
                    self.state.candidate = None;
                }
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
        self.owner.restore_unsaved_locked(&mut self.state).await
    }
}

fn unknown() -> MihomoError {
    MihomoError::Service(zenclash_service::ServiceClientError::Rejected(
        zenclash_service::ServiceErrorCode::OutcomeUnknown,
    ))
}

fn export_status_matches(status: &zenclash_service::ServiceRuntimeStatus, revision: u64) -> bool {
    !status.running
        && status.pid.is_none()
        && status.applied_revision.is_none()
        && status.committed_revision == Some(revision)
        && status.candidate.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    #[tokio::test]
    async fn local_launch_identity_survives_deleted_source_and_service_release() {
        let home = home();
        std::fs::create_dir_all(&home).unwrap();
        let config = home.join("original.yaml");
        std::fs::write(&config, "rules: []\n").unwrap();
        let launch =
            crate::MihomoLaunchConfig::new(home.join("ordinary-mihomo"), &config, &home).unwrap();
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::from_local(service.clone(), launch.clone());
        std::fs::remove_file(&config).unwrap();
        let retained = owner.local_launch().unwrap();
        assert_eq!(retained.binary, launch.binary);
        assert_eq!(retained.config_file, launch.config_file);
        assert_eq!(retained.home_dir, launch.home_dir);
        assert_eq!(owner.source_home(), launch.home_dir);
        assert!(service.calls.lock().is_empty());
        owner.release_owned().await.unwrap();
        assert_eq!(owner.local_launch().unwrap().binary, launch.binary);
        std::fs::remove_dir(home).unwrap();
    }

    #[test]
    fn direct_service_startup_has_no_invented_local_launch() {
        let owner =
            RuntimeSession::new(Arc::new(Service::default()), PathBuf::from("ordinary-home"));
        assert!(owner.local_launch().is_none());
    }

    #[derive(Default)]
    struct Service {
        next: AtomicU64,
        applied: AtomicU64,
        committed: AtomicU64,
        lose_commit_ack: AtomicBool,
        lose_commit_before_apply: AtomicBool,
        lose_reload_ack: AtomicBool,
        lose_status_ack: AtomicBool,
        reject_status: Mutex<Option<zenclash_service::ServiceErrorCode>>,
        reject_stop: Mutex<Option<zenclash_service::ServiceErrorCode>>,
        reject_start: Mutex<Option<zenclash_service::ServiceErrorCode>>,
        calls: Mutex<Vec<String>>,
        running: AtomicBool,
        lose_stop_ack: AtomicBool,
        stop_has_no_effect: AtomicBool,
        lose_release_ack: AtomicBool,
        lose_start_ack: AtomicBool,
        patch_candidate: Mutex<Option<zenclash_service::ServiceRuntimeCandidate>>,
        prepare_delta: Mutex<Option<serde_json::Value>>,
        lose_prepare_ack: AtomicBool,
        reject_apply_patch: Mutex<Option<zenclash_service::ServiceErrorCode>>,
        lose_restore_ack: AtomicBool,
        caches: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
        reject_cache: AtomicBool,
        change_revision_during_cache: AtomicBool,
    }

    impl RuntimeTransport for Service {
        async fn read_cache(
            &self,
            revision: u64,
            _kind: zenclash_service::ProviderKind,
            name: &str,
        ) -> MihomoResult<Option<Vec<u8>>> {
            self.calls
                .lock()
                .push(format!("read-cache:{revision}:{name}"));
            assert!(!self.running.load(Ordering::SeqCst));
            assert_eq!(self.committed.load(Ordering::SeqCst), revision);
            if self.reject_cache.load(Ordering::SeqCst) {
                return Err(unknown());
            }
            if self.change_revision_during_cache.load(Ordering::SeqCst) {
                self.committed.store(revision + 1, Ordering::SeqCst);
            }
            Ok(self.caches.lock().get(name).cloned())
        }
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
            *self.patch_candidate.lock() = None;
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
            if let Some(code) = self.reject_stop.lock().take() {
                return Err(MihomoError::Service(
                    zenclash_service::ServiceClientError::Rejected(code),
                ));
            }
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
            if let Some(code) = self.reject_status.lock().take() {
                return Err(MihomoError::Service(
                    zenclash_service::ServiceClientError::Rejected(code),
                ));
            }
            let (applied_revision, committed_revision) = self.revisions().await?;
            let running = self.running.load(Ordering::SeqCst);
            Ok(zenclash_service::ServiceRuntimeStatus {
                applied_revision,
                committed_revision,
                running,
                pid: running.then_some(123),
                exit_reason: None,
                candidate: self.patch_candidate.lock().clone(),
            })
        }
        async fn start(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("start:{revision}"));
            if let Some(code) = self.reject_start.lock().take() {
                return Err(MihomoError::Service(
                    zenclash_service::ServiceClientError::Rejected(code),
                ));
            }
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
            let stopped = self.stop().await;
            if self.lose_release_ack.swap(false, Ordering::SeqCst) {
                Err(unknown())
            } else {
                stopped
            }
        }
        async fn prepare_patch(
            &self,
            base: u64,
            patch: &serde_json::Value,
        ) -> MihomoResult<zenclash_service::ServicePreparedRuntimePatch> {
            self.calls
                .lock()
                .push(format!("prepare-patch:{base}:{patch}"));
            let revision = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            *self.patch_candidate.lock() = Some(zenclash_service::ServiceRuntimeCandidate {
                revision,
                base_revision: Some(base),
                kind: zenclash_service::ServiceRuntimeCandidateKind::Patch,
                phase: zenclash_service::ServiceRuntimeCandidatePhase::Prepared,
            });
            if self.lose_prepare_ack.swap(false, Ordering::SeqCst) {
                return Err(unknown());
            }
            Ok(zenclash_service::ServicePreparedRuntimePatch {
                revision,
                effective_patch: self
                    .prepare_delta
                    .lock()
                    .take()
                    .unwrap_or_else(|| patch.clone()),
            })
        }
        async fn apply_patch(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("apply-patch:{revision}"));
            if let Some(code) = self.reject_apply_patch.lock().take() {
                return Err(MihomoError::Service(
                    zenclash_service::ServiceClientError::Rejected(code),
                ));
            }
            self.applied.store(revision, Ordering::SeqCst);
            self.patch_candidate.lock().as_mut().unwrap().phase =
                zenclash_service::ServiceRuntimeCandidatePhase::Applied;
            if self.lose_reload_ack.swap(false, Ordering::SeqCst) {
                Err(unknown())
            } else {
                Ok(())
            }
        }
        async fn restore_patch(&self, revision: u64) -> MihomoResult<()> {
            self.calls.lock().push(format!("restore-patch:{revision}"));
            let mut candidate = self.patch_candidate.lock();
            if !self.running.load(Ordering::SeqCst) {
                if candidate.as_ref().map(|candidate| candidate.revision) != Some(revision) {
                    return Err(unknown());
                }
                *candidate = None;
                return if self.lose_restore_ack.swap(false, Ordering::SeqCst) {
                    Err(unknown())
                } else {
                    Ok(())
                };
            }
            let candidate = candidate.as_mut().ok_or_else(unknown)?;
            self.applied
                .store(candidate.base_revision.unwrap(), Ordering::SeqCst);
            candidate.phase = zenclash_service::ServiceRuntimeCandidatePhase::Prepared;
            Ok(())
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

    async fn local_recovery_owner(
        label: &str,
    ) -> (
        crate::core_session::ownership_tests::ChildFixture,
        Arc<Service>,
        Arc<RuntimeSession<Service>>,
        crate::ControlledConfigStore,
    ) {
        let fixture = crate::core_session::ownership_tests::ChildFixture::new(label).await;
        fixture.process.stop_async().await.unwrap();
        let launch = fixture
            .process
            .launch_config()
            .clone()
            .with_controller_endpoint(fixture.process.endpoint().clone());
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::from_local(service.clone(), launch);
        std::fs::write(owner.source_home().join("GeoIP.dat"), b"accepted geoip").unwrap();
        std::fs::write(
            owner.source_home().join("cert.pem"),
            b"fixture public certificate",
        )
        .unwrap();
        std::fs::write(
            owner.source_home().join("nodes.yaml"),
            b"proxies:\n- name: node\n  type: http\n  certificate: cert.pem\n",
        )
        .unwrap();
        owner
            .prepare("tun:\n  enable: true\nproxy-providers:\n  local:\n    type: file\n    path: nodes.yaml\nrule-providers:\n  downloaded:\n    type: http\n    path: downloaded.mrs\n")
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        std::fs::write(
            owner.source_home().join("GeoIP.dat"),
            b"prior ordinary geoip",
        )
        .unwrap();
        std::fs::remove_file(&fixture.process.launch_config().config_file).unwrap();
        std::fs::remove_file(owner.source_home().join("cert.pem")).unwrap();
        std::fs::remove_file(owner.source_home().join("nodes.yaml")).unwrap();
        service.calls.lock().clear();
        let store = crate::ControlledConfigStore::new(owner.source_home().parent().unwrap());
        (fixture, service, owner, store)
    }

    #[tokio::test]
    async fn service_local_recovery_release_precedes_publication_and_shutdown_reaps_new_owner() {
        let (fixture, service, owner, store) =
            local_recovery_owner("geodata-service-recovery-success").await;
        let launch = owner.local_launch().unwrap().clone();
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let published = Arc::new(Mutex::new(None));
        let observed = published.clone();
        let witness = service.clone();
        let outcome = owner
            .recover_local_runtime(
                &store,
                launch,
                Arc::new(AtomicBool::new(false)),
                std::time::Duration::from_secs(2),
                move |process| async move {
                    assert!(witness.calls.lock().iter().any(|call| call == "release"));
                    assert!(process.snapshot().pid.is_none());
                    *observed.lock() = Some(process.clone());
                    client.publish_prepared_process(process, mutation).await
                },
            )
            .await
            .unwrap();
        assert!(outcome.failure.is_none());
        let process = published.lock().as_ref().unwrap().clone();
        assert!(process.snapshot().pid.is_some());
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(&process.launch_config().config_file).unwrap())
                .unwrap();
        assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(
            store.load().unwrap()["tun"]["enable"].as_bool(),
            Some(false)
        );
        let cached: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(store.runtime_path()).unwrap()).unwrap();
        assert_eq!(
            cached, yaml,
            "restart cache must reference the ordinary recovery slot"
        );
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"accepted geoip"
        );
        let next_owner = RuntimeSession::from_local(
            Arc::new(Service::default()),
            owner.local_launch().unwrap().clone(),
        );
        let cache_path = yaml["rule-providers"]["downloaded"]["path"]
            .as_str()
            .unwrap();
        std::fs::write(cache_path, b"downloaded during local recovery").unwrap();
        // Production reads after Stop; the ordinary child fixture exercises the same boundary.
        process.stop_async().await.unwrap();
        let refreshed = outcome
            .bundle
            .export_local_caches(&store, process.launch_config().config_file.clone())
            .await
            .unwrap();
        let next_bundle = Arc::new(
            refreshed
                .with_delta(&serde_json::json!({"tun":{"enable":true}}))
                .unwrap(),
        );
        next_owner
            .prepare_bundle(next_bundle.clone())
            .await
            .unwrap()
            .apply(true)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            serde_yaml::from_str::<serde_yaml::Value>(next_owner.snapshot().unwrap().yaml())
                .unwrap()["tun"]["enable"]
                .as_bool(),
            Some(true)
        );
        let next_config = next_bundle
            .materialize_local_runtime(&store, Some(process.launch_config().config_file.clone()))
            .await
            .unwrap();
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(&next_config).unwrap()).unwrap();
        assert_eq!(
            std::fs::read(
                yaml["rule-providers"]["downloaded"]["path"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            b"downloaded during local recovery"
        );
        let provider: serde_yaml::Value = serde_yaml::from_slice(
            &std::fs::read(yaml["proxy-providers"]["local"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(provider["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
            b"fixture public certificate"
        );
        process
            .restart_and_wait_until_with_lease(std::time::Duration::from_secs(3), None, &lease)
            .await
            .unwrap();
        assert!(process.snapshot().pid.is_some());
        session.shutdown().await.unwrap();
        assert!(process.snapshot().pid.is_none());
    }

    #[tokio::test]
    async fn initial_handover_recovery_uses_held_resources_after_sources_are_deleted() {
        let (fixture, _, accepted, store) =
            local_recovery_owner("geodata-initial-handover-recovery").await;
        let bundle = accepted.snapshot().unwrap();
        let service = Arc::new(Service::default());
        let owner =
            RuntimeSession::from_local(service.clone(), accepted.local_launch().unwrap().clone());
        assert!(
            owner.snapshot().is_err(),
            "the failed trial has no accepted runtime"
        );
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let published = Arc::new(Mutex::new(None));
        let observed = published.clone();
        let store_mutation = store.lock_service_tun_mutation().await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            owner.recover_held_local_runtime(
                (&store, store_mutation),
                owner.local_launch().unwrap().clone(),
                bundle,
                Arc::new(AtomicBool::new(false)),
                std::time::Duration::from_secs(2),
                move |process| async move {
                    assert!(service.calls.lock().iter().any(|call| call == "release"));
                    assert!(process.snapshot().pid.is_none());
                    *observed.lock() = Some(process.clone());
                    client.publish_prepared_process(process, mutation).await
                },
            ),
        )
        .await
        .expect("held store admission must not self-lock")
        .unwrap();
        assert!(outcome.failure.is_none());
        let process = published.lock().as_ref().unwrap().clone();
        assert!(process.snapshot().pid.is_some());
        assert_eq!(process.launch_config().home_dir, owner.source_home());
        let payload = std::fs::read(&process.launch_config().config_file).unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_slice(&payload).unwrap();
        assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(std::fs::read(store.runtime_path()).unwrap(), payload);
        let provider: serde_yaml::Value = serde_yaml::from_slice(
            &std::fs::read(yaml["proxy-providers"]["local"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(provider["proxies"][0]["certificate"].as_str().unwrap()).unwrap(),
            b"fixture public certificate"
        );
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"accepted geoip"
        );
        assert!(!fixture.process.launch_config().config_file.exists());
        session.shutdown().await.unwrap();
        assert!(process.snapshot().pid.is_none());
    }

    #[tokio::test]
    async fn initial_handover_recovery_unknown_release_preserves_home_without_publication() {
        let (fixture, _, accepted, store) =
            local_recovery_owner("geodata-initial-handover-release-unknown").await;
        let bundle = accepted.snapshot().unwrap();
        let service = Arc::new(Service::default());
        service.lose_release_ack.store(true, Ordering::SeqCst);
        let owner = RuntimeSession::from_local(service, accepted.local_launch().unwrap().clone());
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        let published = Arc::new(AtomicBool::new(false));
        let observed = published.clone();
        assert!(
            owner
                .recover_held_local_runtime(
                    (&store, store.lock_service_tun_mutation().await),
                    owner.local_launch().unwrap().clone(),
                    bundle,
                    Arc::new(AtomicBool::new(false)),
                    std::time::Duration::from_millis(200),
                    move |_| async move {
                        observed.store(true, Ordering::SeqCst);
                        Ok(Arc::new(tokio::sync::Mutex::new(())).lock_owned().await)
                    },
                )
                .await
                .is_err()
        );
        assert!(!published.load(Ordering::SeqCst));
        assert!(fixture.process.snapshot().pid.is_none());
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"prior ordinary geoip"
        );
    }

    #[tokio::test]
    async fn service_local_recovery_unknown_release_never_publishes_or_starts_local() {
        let (fixture, service, owner, store) =
            local_recovery_owner("geodata-service-recovery-release-unknown").await;
        let launch = owner.local_launch().unwrap().clone();
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        service.lose_release_ack.store(true, Ordering::SeqCst);
        let published = Arc::new(AtomicBool::new(false));
        let observed = published.clone();
        assert!(
            owner
                .recover_local_runtime(
                    &store,
                    launch,
                    Arc::new(AtomicBool::new(false)),
                    std::time::Duration::from_millis(200),
                    move |_| async move {
                        observed.store(true, Ordering::SeqCst);
                        Ok(Arc::new(tokio::sync::Mutex::new(())).lock_owned().await)
                    }
                )
                .await
                .is_err()
        );
        assert!(!published.load(Ordering::SeqCst));
        assert!(fixture.process.snapshot().pid.is_none());
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"prior ordinary geoip"
        );
    }

    #[tokio::test]
    async fn service_local_recovery_save_failure_keeps_running_owner_and_activated_resources() {
        let (fixture, _, owner, store) =
            local_recovery_owner("geodata-service-recovery-save-failure").await;
        let launch = owner.local_launch().unwrap().clone();
        let previous = b"tun:\n  enable: true\n";
        std::fs::write(store.runtime_path(), previous).unwrap();
        std::fs::create_dir(store.root().join("override.yaml")).unwrap();
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let published = Arc::new(Mutex::new(None));
        let observed = published.clone();
        let outcome = owner
            .recover_local_runtime(
                &store,
                launch,
                Arc::new(AtomicBool::new(false)),
                std::time::Duration::from_secs(2),
                move |process| async move {
                    *observed.lock() = Some(process.clone());
                    client.publish_prepared_process(process, mutation).await
                },
            )
            .await
            .unwrap();
        assert!(
            outcome.failure.is_some(),
            "saving failure must prohibit maintenance"
        );
        let process = published.lock().as_ref().unwrap().clone();
        assert!(
            process.snapshot().pid.is_some(),
            "the safe ordinary owner remains available to shutdown"
        );
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"accepted geoip"
        );
        assert_eq!(std::fs::read(store.runtime_path()).unwrap(), previous);
        session.shutdown().await.unwrap();
        assert!(process.snapshot().pid.is_none());
    }

    #[tokio::test]
    async fn service_local_recovery_readiness_failure_keeps_stopped_owner_and_rolls_back_geodata() {
        let (fixture, _, owner, store) =
            local_recovery_owner("geodata-service-recovery-readiness-failure").await;
        fixture.responder_for_test_abort();
        let launch = owner.local_launch().unwrap().clone();
        let lease = store
            .acquire_write_lease_for_paths(fixture.process.write_scopes())
            .await
            .unwrap();
        let store = store.with_write_lease(&lease);
        let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
        let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
        let mutation = client.lock_runtime_binding().await.unwrap();
        let published = Arc::new(Mutex::new(None));
        let observed = published.clone();
        assert!(
            owner
                .recover_local_runtime(
                    &store,
                    launch,
                    Arc::new(AtomicBool::new(false)),
                    std::time::Duration::from_millis(100),
                    move |process| async move {
                        *observed.lock() = Some(process.clone());
                        client.publish_prepared_process(process, mutation).await
                    }
                )
                .await
                .is_err()
        );
        assert!(published.lock().as_ref().unwrap().snapshot().pid.is_none());
        assert_eq!(
            std::fs::read(owner.source_home().join("GeoIP.dat")).unwrap(),
            b"prior ordinary geoip"
        );
        session.shutdown().await.unwrap();
    }

    async fn accepted_cache_owner() -> (Arc<Service>, Arc<RuntimeSession<Service>>) {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare(
                "rule-providers:\n  rules:\n    type: http\n    url: https://example.com/rules\n",
            )
            .await
            .unwrap()
            .apply(false)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        service
            .caches
            .lock()
            .insert("rules".into(), b"downloaded cache".to_vec());
        service.calls.lock().clear();
        (service, owner)
    }

    #[tokio::test]
    async fn stopped_export_rejects_revision_change_after_last_cache_read() {
        let (service, owner) = accepted_cache_owner().await;
        let before = owner.snapshot().unwrap();
        service
            .change_revision_during_cache
            .store(true, Ordering::SeqCst);
        assert!(owner.stop_and_export().await.is_err());
        assert!(Arc::ptr_eq(&before, &owner.snapshot().unwrap()));
    }

    #[tokio::test]
    async fn stopped_export_confirms_stop_and_only_then_reads_accepted_cache() {
        let (service, owner) = accepted_cache_owner().await;
        let before = owner.snapshot().unwrap();
        let exported = owner.stop_and_export().await.unwrap();
        assert!(!service.running.load(Ordering::SeqCst));
        assert!(!Arc::ptr_eq(&before, &exported));
        assert!(Arc::ptr_eq(&exported, &owner.snapshot().unwrap()));
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "status", "read-cache:1:rules", "status"]
        );
    }

    #[tokio::test]
    async fn stopped_export_failure_preserves_accepted_bundle_without_restart_or_release() {
        let (service, owner) = accepted_cache_owner().await;
        let before = owner.snapshot().unwrap();
        service.reject_cache.store(true, Ordering::SeqCst);
        assert!(owner.stop_and_export().await.is_err());
        assert!(Arc::ptr_eq(&before, &owner.snapshot().unwrap()));
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "status", "read-cache:1:rules"]
        );
    }

    #[tokio::test]
    async fn stopped_export_unconfirmed_stop_never_reads_cache() {
        let (service, owner) = accepted_cache_owner().await;
        service.stop_has_no_effect.store(true, Ordering::SeqCst);
        assert!(owner.stop_and_export().await.is_err());
        assert_eq!(*service.calls.lock(), ["stop", "status"]);
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
    async fn restart_start_policy_rejection_after_stop_is_an_unknown_mutation() {
        let (service, owner) = accepted_owner().await;
        *service.reject_start.lock() = Some(zenclash_service::ServiceErrorCode::KernelUnavailable);
        service.calls.lock().clear();
        let error = owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap_err();
        assert!(!service.running.load(Ordering::SeqCst));
        assert_eq!(
            *service.calls.lock(),
            ["stop", "status", "start:1", "status"]
        );
        assert!(
            error.mutation_result_unknown(),
            "Start refusal cannot undo the successful Stop"
        );
    }

    #[tokio::test]
    async fn restart_without_an_accepted_runtime_is_rejected_before_any_mutation() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        let error = owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap_err();
        assert!(service.calls.lock().is_empty());
        assert!(!error.mutation_result_unknown());
    }

    #[tokio::test]
    async fn status_policy_rejection_after_stop_is_an_unknown_mutation() {
        let (service, owner) = accepted_owner().await;
        *service.reject_status.lock() = Some(zenclash_service::ServiceErrorCode::Expired);
        let error = owner.stop_confirmed().await.unwrap_err();
        assert!(!service.running.load(Ordering::SeqCst));
        assert!(
            error.mutation_result_unknown(),
            "Stop already changed the child before Status failed"
        );
    }

    #[tokio::test]
    async fn stop_policy_rejection_before_effect_keeps_its_known_error() {
        let (service, owner) = accepted_owner().await;
        *service.reject_stop.lock() = Some(zenclash_service::ServiceErrorCode::Expired);
        *service.reject_status.lock() = Some(zenclash_service::ServiceErrorCode::Expired);
        let error = owner.stop_confirmed().await.unwrap_err();
        assert!(service.running.load(Ordering::SeqCst));
        assert!(!error.mutation_result_unknown());
    }

    #[tokio::test]
    async fn uncertain_stop_is_not_reversed_after_a_fresh_stopped_status() {
        let (service, owner) = accepted_owner().await;
        service.lose_stop_ack.store(true, Ordering::SeqCst);
        service.lose_status_ack.store(true, Ordering::SeqCst);
        let result = owner
            .stop_confirmed()
            .await
            .map_err(crate::CoreSessionError::from);
        assert!(result.is_err());
        let mut lifecycle = crate::CoreLifecycleSnapshot {
            phase: crate::CoreLifecyclePhase::Stable,
            stop_requested: false,
            recovery_attempts: 0,
            exit_reason: None,
            last_error: None,
        };
        crate::core_session::record_maintenance_outcome(
            &mut lifecycle,
            crate::CoreMaintenanceIntent::Stop,
            &result,
        );
        assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Unknown);
        service.calls.lock().clear();
        let status = service.status().await.unwrap();
        assert!(!status.running);
        if crate::core_session::supervisor_observation(
            &mut lifecycle,
            Some((status.running, status.exit_reason)),
            false,
            false,
            crate::CoreKind::Mihomo,
            3,
        )
        .is_some_and(|(_, exhausted)| !exhausted)
        {
            owner
                .restart_accepted(&AtomicBool::new(false))
                .await
                .unwrap();
        }
        assert_eq!(
            service
                .calls
                .lock()
                .iter()
                .filter(|call| call.starts_with("start:"))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn stopped_owner_status_failure_never_authorizes_supervisor_start() {
        let (service, owner) = accepted_owner().await;
        owner.stop_confirmed().await.unwrap();
        let mut lifecycle = crate::CoreLifecycleSnapshot {
            phase: crate::CoreLifecyclePhase::Stopped,
            stop_requested: true,
            recovery_attempts: 0,
            exit_reason: None,
            last_error: None,
        };
        service.calls.lock().clear();
        service.lose_status_ack.store(true, Ordering::SeqCst);
        for _ in 0..2 {
            let observed = service
                .status()
                .await
                .ok()
                .map(|status| (status.running, status.exit_reason));
            if crate::core_session::supervisor_observation(
                &mut lifecycle,
                observed,
                false,
                false,
                crate::CoreKind::Mihomo,
                3,
            )
            .is_some_and(|(_, exhausted)| !exhausted)
            {
                owner
                    .restart_accepted(&AtomicBool::new(false))
                    .await
                    .unwrap();
            }
        }
        assert_eq!(
            service
                .calls
                .lock()
                .iter()
                .filter(|call| call.starts_with("start:"))
                .count(),
            0
        );
        assert!(!service.running.load(Ordering::SeqCst));
        assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Stopped);
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
                .filter(|call| call.starts_with("start:"))
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
                .filter(|call| call.starts_with("start:"))
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
    #[tokio::test]
    async fn partial_service_patch_shares_held_resources_and_uses_effective_delta() {
        let home = home();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("provider.yaml"), "payload: [example.org]\n").unwrap();
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home.clone());
        owner.prepare("mode: rule\ntun: {enable: true, mtu: 1400}\nrule-providers:\n  rules:\n    type: file\n    behavior: domain\n    path: provider.yaml\n").await.unwrap().apply(true).await.unwrap().commit().await.unwrap();
        let before = owner.snapshot().unwrap();
        std::fs::remove_dir_all(&home).unwrap();
        *service.prepare_delta.lock() = Some(serde_json::json!({"tun":{"mtu":1500,"enable":true}}));
        service.calls.lock().clear();
        let prepared = owner
            .prepare_patch(&serde_json::json!({"tun":{"mtu":1500}}))
            .await
            .unwrap();
        assert_eq!(prepared.effective_delta().unwrap()["tun"]["enable"], true);
        prepared.apply(false).await.unwrap().commit().await.unwrap();
        let after = owner.snapshot().unwrap();
        let before_value: serde_yaml::Value = serde_yaml::from_str(before.yaml()).unwrap();
        let after_value: serde_yaml::Value = serde_yaml::from_str(after.yaml()).unwrap();
        assert_eq!(
            before_value["rule-providers"],
            after_value["rule-providers"]
        );
        assert_eq!(after_value["tun"]["enable"], true);
        assert_eq!(after_value["tun"]["mtu"], 1500);
        assert_eq!(service.calls.lock().len(), 3);
        assert!(
            service
                .calls
                .lock()
                .iter()
                .all(|call| !call.starts_with("stage:")
                    && !call.starts_with("reload:")
                    && !call.starts_with("validate:"))
        );
    }

    #[tokio::test]
    async fn partial_service_saved_commit_ack_loss_only_finalizes() {
        let (service, owner) = accepted_owner().await;
        service.calls.lock().clear();
        let applied = owner
            .prepare_patch(&serde_json::json!({"mode":"global"}))
            .await
            .unwrap()
            .apply(false)
            .await
            .unwrap();
        service
            .lose_commit_before_apply
            .store(true, Ordering::SeqCst);
        assert!(
            applied
                .commit()
                .await
                .unwrap_err()
                .mutation_result_unknown()
        );
        assert!(owner.restore_active().await.is_err());
        owner.reconcile().await.unwrap();
        assert!(owner.snapshot().unwrap().yaml().contains("mode: global"));
        let calls = service.calls.lock().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("apply-patch:"))
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
        assert!(!calls.iter().any(|call| call.starts_with("stage:")
            || call.starts_with("restore-patch:")
            || call.starts_with("reload:")));
    }

    #[tokio::test]
    async fn partial_service_unsaved_rollback_is_inverse_not_reload_even_after_lost_commit() {
        let (service, owner) = accepted_owner().await;
        service.calls.lock().clear();
        let applied = owner
            .prepare_patch(&serde_json::json!({"mode":"global"}))
            .await
            .unwrap()
            .apply(false)
            .await
            .unwrap();
        service.lose_commit_ack.store(true, Ordering::SeqCst);
        assert!(applied.rollback().await.is_err());
        owner.restore_active().await.unwrap();
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
        let calls = service.calls.lock().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("restore-patch:"))
                .count(),
            1
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("reload:") || call.starts_with("stage:"))
        );
    }

    #[tokio::test]
    async fn partial_service_lost_apply_ack_never_resends_and_explicit_recovery_restores_base() {
        let (service, owner) = accepted_owner().await;
        service.calls.lock().clear();
        service.lose_reload_ack.store(true, Ordering::SeqCst);
        assert!(
            owner
                .prepare_patch(&serde_json::json!({"mode":"global"}))
                .await
                .unwrap()
                .apply(false)
                .await
                .is_err()
        );
        assert!(owner.prepare("mode: direct\n").await.is_err());
        owner.restore_active().await.unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        let calls = service.calls.lock().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("apply-patch:"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("restore-patch:"))
                .count(),
            1
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("stage:") || call.starts_with("reload:"))
        );
    }

    #[tokio::test]
    async fn partial_service_lost_prepare_ack_uses_status_before_clearing_candidate() {
        let (service, owner) = accepted_owner().await;
        service.calls.lock().clear();
        service.lose_prepare_ack.store(true, Ordering::SeqCst);
        assert!(
            owner
                .prepare_patch(&serde_json::json!({"mode":"global"}))
                .await
                .is_err()
        );
        assert!(owner.snapshot().is_err());
        owner.reconcile().await.unwrap();
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
        let calls = service.calls.lock().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("prepare-patch:"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("restore-patch:"))
                .count(),
            1
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("apply-patch:") || call.starts_with("stage:"))
        );
    }
    #[tokio::test]
    async fn stopped_partial_prepared_candidate_can_restart_only_the_accepted_revision() {
        let (service, owner) = accepted_owner().await;
        let prepared = owner
            .prepare_patch(&serde_json::json!({"mode":"global"}))
            .await
            .unwrap();
        service.stop().await.unwrap();
        *service.reject_apply_patch.lock() =
            Some(zenclash_service::ServiceErrorCode::KernelUnavailable);
        assert!(prepared.apply(false).await.is_err());
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
        let calls = service.calls.lock().clone();
        assert!(!calls.iter().any(|call| call.starts_with("apply-patch:")
            || call.starts_with("reload:")
            || call.starts_with("stage:")
            || call == "start:2"));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.as_str() == "start:1")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stopped_partial_unknown_preparation_can_restart_without_repreparing() {
        let (service, owner) = accepted_owner().await;
        service.lose_prepare_ack.store(true, Ordering::SeqCst);
        assert!(
            owner
                .prepare_patch(&serde_json::json!({"mode":"global"}))
                .await
                .is_err()
        );
        service.stop().await.unwrap();
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(service.applied.load(Ordering::SeqCst), 1);
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
        let calls = service.calls.lock().clone();
        assert!(!calls.iter().any(|call| call.starts_with("prepare-patch:")
            || call.starts_with("apply-patch:")
            || call.starts_with("stage:")));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.as_str() == "start:1")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stopped_partial_lost_restore_ack_is_confirmed_before_accepted_start() {
        let (service, owner) = accepted_owner().await;
        drop(
            owner
                .prepare_patch(&serde_json::json!({"mode":"global"}))
                .await
                .unwrap(),
        );
        service.stop().await.unwrap();
        service.lose_restore_ack.store(true, Ordering::SeqCst);
        service.calls.lock().clear();
        owner
            .restart_accepted(&AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            [
                "status",
                "restore-patch:2",
                "status",
                "stop",
                "status",
                "start:1",
                "status"
            ]
        );
        assert!(owner.snapshot().unwrap().yaml().contains("mode: rule"));
    }

    #[tokio::test]
    async fn stopped_partial_saved_finalizing_receipt_never_restores_or_starts() {
        let (service, owner) = accepted_owner().await;
        service
            .lose_commit_before_apply
            .store(true, Ordering::SeqCst);
        assert!(
            owner
                .prepare_patch(&serde_json::json!({"mode":"global"}))
                .await
                .unwrap()
                .apply(false)
                .await
                .unwrap()
                .commit()
                .await
                .is_err()
        );
        service.stop().await.unwrap();
        service.calls.lock().clear();
        assert!(
            owner
                .restart_accepted(&AtomicBool::new(false))
                .await
                .is_err()
        );
        assert!(owner.restore_active().await.is_err());
        let calls = service.calls.lock().clone();
        assert!(!calls.iter().any(|call| call.starts_with("restore-patch:")
            || call.starts_with("start:")
            || call.starts_with("commit:")));
    }
    #[tokio::test]
    async fn initial_service_runtime_starts_validated_revision_before_commit() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(false)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            [
                "stage:1:mode: rule\n",
                "validate:1",
                "start:1",
                "status",
                "commit:1"
            ]
        );
        assert!(owner.snapshot().is_ok());
    }

    #[tokio::test]
    async fn initial_service_lost_start_ack_uses_readback_without_repeat_start() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        service.lose_start_ack.store(true, Ordering::SeqCst);
        owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(false)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            *service.calls.lock(),
            [
                "stage:1:mode: rule\n",
                "validate:1",
                "start:1",
                "status",
                "commit:1"
            ]
        );
    }

    #[tokio::test]
    async fn initial_service_unverified_start_retains_owner_for_shutdown() {
        let service = Arc::new(Service::default());
        let owner = RuntimeSession::new(service.clone(), home());
        *service.reject_status.lock() = Some(zenclash_service::ServiceErrorCode::KernelUnavailable);
        let result = owner
            .prepare("mode: rule\n")
            .await
            .unwrap()
            .apply(false)
            .await;
        assert!(
            result.is_err(),
            "unverified Start must not yield an applied receipt"
        );
        let error = result.err().unwrap();
        assert!(error.mutation_result_unknown());
        assert!(owner.snapshot().is_err());
        owner.release_owned().await.unwrap();
        let calls = service.calls.lock().clone();
        assert!(calls.iter().any(|call| call == "release"));
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("reload:") || call.starts_with("commit:"))
        );
        assert!(!service.running.load(Ordering::SeqCst));
    }
}
