//! Application-owned persistence and rollback around upstream native runtime operations.

use crate::service::{NativeHttpResponse, ServiceCallError, ServiceSession};
use crate::{
    MihomoError, MihomoResult,
    service_runtime::{FrozenRuntime, ServiceRuntimeBundle},
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zenclash_service::{
    RuntimeFileOutcome, RuntimeFileRequest, ServiceStatusSnapshot, StageRuntimeOutcome,
};

mod cache;
mod local_recovery;

/// The methods are the actual fork/controller operations, with no synthetic service revisions.
pub(crate) trait RuntimeTransport: Send + Sync + 'static {
    fn prepare_snapshot(
        &self,
        bundle: Arc<ServiceRuntimeBundle>,
        home: PathBuf,
        core: Option<PathBuf>,
    ) -> impl Future<Output = MihomoResult<FrozenRuntime>> + Send;
    fn start(
        &self,
        runtime: zenclash_service::RuntimeBundle,
    ) -> impl Future<Output = MihomoResult<()>> + Send;
    fn stage_runtime(
        &self,
        runtime: &zenclash_service::RuntimeBundle,
    ) -> impl Future<Output = MihomoResult<StageRuntimeOutcome>> + Send;
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        secret: &str,
    ) -> impl Future<Output = MihomoResult<NativeHttpResponse>> + Send;
    fn stop(&self) -> impl Future<Output = MihomoResult<()>> + Send;
    fn status(&self) -> impl Future<Output = MihomoResult<ServiceStatusSnapshot>> + Send;
    fn owns_status(&self, status: &ServiceStatusSnapshot) -> bool;
    fn active_generation(&self) -> Option<u64>;
    fn read_runtime_file(
        &self,
        request: &RuntimeFileRequest,
    ) -> impl Future<Output = MihomoResult<RuntimeFileOutcome>> + Send;
}

impl RuntimeTransport for ServiceSession {
    async fn prepare_snapshot(
        &self,
        bundle: Arc<ServiceRuntimeBundle>,
        home: PathBuf,
        core: Option<PathBuf>,
    ) -> MihomoResult<FrozenRuntime> {
        FrozenRuntime::prepare(bundle, home, core, true).await
    }
    async fn start(&self, runtime: zenclash_service::RuntimeBundle) -> MihomoResult<()> {
        ServiceSession::start(self, runtime).await?;
        Ok(())
    }
    async fn stage_runtime(
        &self,
        runtime: &zenclash_service::RuntimeBundle,
    ) -> MihomoResult<StageRuntimeOutcome> {
        Ok(ServiceSession::stage_runtime(self, runtime).await?)
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        secret: &str,
    ) -> MihomoResult<NativeHttpResponse> {
        Ok(self
            .controller_request(method, path, body, secret, Duration::from_secs(12))
            .await?)
    }
    async fn stop(&self) -> MihomoResult<()> {
        Ok(ServiceSession::stop(self).await?)
    }
    async fn status(&self) -> MihomoResult<ServiceStatusSnapshot> {
        Ok(ServiceSession::status(self).await?)
    }
    fn owns_status(&self, status: &ServiceStatusSnapshot) -> bool {
        ServiceSession::owns_status(self, status)
    }
    fn active_generation(&self) -> Option<u64> {
        self.active_proof().ok().map(|proof| proof.generation)
    }
    async fn read_runtime_file(
        &self,
        request: &RuntimeFileRequest,
    ) -> MihomoResult<RuntimeFileOutcome> {
        Ok(ServiceSession::read_runtime_file(self, request).await?)
    }
}

pub(crate) type ServiceRuntimeSession = RuntimeSession<ServiceSession>;

pub(crate) struct RuntimeSession<T: RuntimeTransport> {
    pub(crate) client: Arc<T>,
    source_home: PathBuf,
    core_source: Option<PathBuf>,
    controller_secret: parking_lot::RwLock<String>,
    local_launch: Option<crate::MihomoLaunchConfig>,
    state: Arc<Mutex<RuntimeState>>,
}

#[derive(Clone)]
struct RuntimeSnapshot {
    bundle: Arc<ServiceRuntimeBundle>,
    frozen: FrozenRuntime,
    delta: Option<serde_json::Value>,
}

#[derive(Default)]
struct RuntimeState {
    active: Option<RuntimeSnapshot>,
    candidate: Option<(RuntimeSnapshot, CandidatePhase)>,
    closing: bool,
    released: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidatePhase {
    Prepared,
    Uncertain,
    Applied,
    // The caller has durably saved the cache. Only finishing this candidate is allowed.
    Finalizing,
}

pub(crate) struct PreparedRuntime<T: RuntimeTransport = ServiceSession> {
    owner: Arc<RuntimeSession<T>>,
    state: OwnedMutexGuard<RuntimeState>,
    recovery: bool,
}

pub(crate) struct AppliedRuntime<T: RuntimeTransport = ServiceSession> {
    owner: Arc<RuntimeSession<T>>,
    state: OwnedMutexGuard<RuntimeState>,
}

impl<T: RuntimeTransport> RuntimeSession<T> {
    pub(crate) fn new(client: Arc<T>, source_home: PathBuf) -> Arc<Self> {
        Self::with_core_source(client, source_home, None)
    }
    pub(crate) fn with_core_source(
        client: Arc<T>,
        source_home: PathBuf,
        core_source: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            source_home,
            core_source,
            controller_secret: Default::default(),
            local_launch: None,
            state: Arc::new(Mutex::new(RuntimeState::default())),
        })
    }
    pub(crate) fn from_local(client: Arc<T>, launch: crate::MihomoLaunchConfig) -> Arc<Self> {
        Arc::new(Self {
            client,
            source_home: launch.home_dir.clone(),
            core_source: Some(launch.binary.clone()),
            controller_secret: Default::default(),
            local_launch: Some(launch),
            state: Arc::new(Mutex::new(RuntimeState::default())),
        })
    }
    pub(crate) fn local_launch(&self) -> Option<&crate::MihomoLaunchConfig> {
        self.local_launch.as_ref()
    }
    pub(crate) fn core_source(&self) -> Option<&Path> {
        self.core_source.as_deref()
    }
    pub(crate) fn source_home(&self) -> &Path {
        &self.source_home
    }
    pub(crate) fn controller_secret(&self) -> String {
        self.controller_secret.read().clone()
    }

    async fn freeze(
        &self,
        bundle: Arc<ServiceRuntimeBundle>,
        delta: Option<serde_json::Value>,
    ) -> MihomoResult<RuntimeSnapshot> {
        let core = self.core_source.clone();
        let frozen = self
            .client
            .prepare_snapshot(bundle.clone(), self.source_home.clone(), core)
            .await?;
        Ok(RuntimeSnapshot {
            bundle,
            frozen,
            delta,
        })
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        payload: &str,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        let bundle =
            Arc::new(ServiceRuntimeBundle::prepare(payload, self.source_home.clone()).await?);
        self.prepare_locked(bundle, None, state, false).await
    }
    pub(crate) async fn prepare_bundle(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        self.prepare_locked(bundle, None, state, false).await
    }
    pub(crate) async fn prepare_patch(
        self: &Arc<Self>,
        patch: &serde_json::Value,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        self.reconcile_locked(&mut state).await?;
        let active = state.active.as_ref().ok_or_else(no_snapshot)?;
        // Patch preparation changes no service files. Preserve accepted asset bytes.
        let bundle = Arc::new(active.bundle.with_delta(patch)?);
        self.prepare_locked(bundle, Some(patch.clone()), state, false)
            .await
    }
    pub(crate) async fn prepare_recovery(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        let mut state = self.state.clone().lock_owned().await;
        if state.closing || state.released {
            return Err(unknown());
        }
        if matches!(state.candidate, Some((_, CandidatePhase::Finalizing))) {
            return Err(unknown());
        }
        // Unsaved state must be restored before a replacement can retire its files.
        self.restore_unsaved_locked(&mut state).await?;
        self.prepare_locked(bundle, None, state, true).await
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
                // Keep backup admission alive while native confirmation is awaited.
                let _admission = admission.clone();
                self.prepare_backup_recovery(bundle).await
            }
        }
    }
    pub(crate) async fn prepare_backup_recovery(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
    ) -> MihomoResult<PreparedRuntime<T>> {
        {
            let mut state = self.state.lock().await;
            self.confirm_finalizing_locked(&mut state).await?;
        }
        self.prepare_recovery(bundle).await
    }
    async fn prepare_locked(
        self: &Arc<Self>,
        bundle: Arc<ServiceRuntimeBundle>,
        delta: Option<serde_json::Value>,
        mut state: OwnedMutexGuard<RuntimeState>,
        recovery: bool,
    ) -> MihomoResult<PreparedRuntime<T>> {
        if state.closing || state.released || state.candidate.is_some() {
            return Err(unknown());
        }
        let candidate = self.freeze(bundle, delta).await?;
        state.candidate = Some((candidate, CandidatePhase::Prepared));
        Ok(PreparedRuntime {
            owner: self.clone(),
            state,
            recovery,
        })
    }

    pub(crate) fn observation_allowed(&self) -> bool {
        self.state
            .try_lock()
            .is_ok_and(|state| !state.closing && !state.released && state.candidate.is_none())
    }
    pub(crate) fn snapshot(&self) -> MihomoResult<Arc<ServiceRuntimeBundle>> {
        let state = self.state.try_lock().map_err(|_| unknown())?;
        if state.closing || state.candidate.is_some() {
            return Err(unknown());
        }
        state
            .active
            .as_ref()
            .map(|active| active.bundle.clone())
            .ok_or_else(no_snapshot)
    }
    pub(crate) async fn reconcile(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        self.reconcile_locked(&mut state).await
    }
    async fn reconcile_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        if state.closing || state.released {
            return Err(unknown());
        }
        match state.candidate.as_ref().map(|(_, phase)| *phase) {
            None => Ok(()),
            Some(CandidatePhase::Prepared) => {
                state.candidate = None;
                Ok(())
            }
            Some(CandidatePhase::Finalizing) => {
                // A previous confirmation may have failed after persistence. Reapply the
                // saved snapshot; native status has no applied/committed revision fields.
                let candidate = state.candidate.as_ref().ok_or_else(unknown)?.0.clone();
                self.load_snapshot(&candidate, true, false).await?;
                self.confirm_controller().await?;
                state.active = state.candidate.take().map(|(snapshot, _)| snapshot);
                Ok(())
            }
            Some(CandidatePhase::Applied | CandidatePhase::Uncertain) => Err(unknown()),
        }
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

    async fn confirm_controller(&self) -> MihomoResult<()> {
        let status = self.client.status().await?;
        if !self.client.owns_status(&status) || status.core_pid.is_none() {
            return Err(unknown());
        }
        let response = self
            .client
            .request("GET", "/configs", None, &self.controller_secret())
            .await?;
        ensure_success(response)?;
        Ok(())
    }

    async fn load_snapshot(
        &self,
        snapshot: &RuntimeSnapshot,
        force: bool,
        restart: bool,
    ) -> MihomoResult<()> {
        let secret = snapshot.bundle.controller_secret()?;
        if !restart && self.client.active_generation().is_some() {
            match self.client.stage_runtime(&snapshot.frozen.native).await {
                Ok(StageRuntimeOutcome::Staged { config_path }) => {
                    let body = serde_json::json!({"path":config_path});
                    let path = if force {
                        "/configs?force=true"
                    } else {
                        "/configs?force=false"
                    };
                    // Authentication uses the running snapshot's secret until load succeeds.
                    let loaded = async {
                        let response = self
                            .client
                            .request("PUT", path, Some(&body), &self.controller_secret())
                            .await?;
                        ensure_success(response)?;
                        *self.controller_secret.write() = secret;
                        self.confirm_controller().await
                    }
                    .await;
                    return loaded.map_err(partially_applied);
                }
                Ok(StageRuntimeOutcome::RestartRequired { .. }) => {}
                Err(MihomoError::Service(ServiceCallError::UnsupportedCapability(
                    "runtime staging",
                ))) => {}
                Err(error) => return Err(partially_applied(error)),
            }
        }
        let stopped_previous = self.client.active_generation().is_some();
        if stopped_previous {
            self.confirm_stop().await?;
        }
        let started = self.client.start(snapshot.frozen.native.clone()).await;
        if let Err(error) = started {
            return Err(if stopped_previous {
                partially_applied(error)
            } else {
                error
            });
        }
        *self.controller_secret.write() = secret;
        self.confirm_controller().await.map_err(partially_applied)
    }

    async fn confirm_stop(&self) -> MihomoResult<()> {
        let generation = self.client.active_generation();
        let stopped = self.client.stop().await;
        // A protected native Stop acknowledgement already confirms retirement.
        // If it was lost, a later status may prove the captured generation is gone.
        if stopped.is_ok() {
            return Ok(());
        }
        let status = self.client.status().await.map_err(|_| unknown())?;
        if !status.is_active
            || generation.is_some_and(|generation| status.active_generation != Some(generation))
        {
            return Ok(());
        }
        stopped.and(Err(unknown()))
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
        if state.candidate.is_some() {
            return Err(unknown());
        }
        let active = state.active.as_ref().ok_or_else(no_snapshot)?;
        let status = self.client.status().await?;
        if !self.client.owns_status(&status) || status.core_pid.is_none() {
            return Err(unknown());
        }
        let providers = active.bundle.cache_providers()?;
        let mut bundle = active.bundle.cache_export_base(&providers);
        // The native Stop clears proof. Cache reads must finish while it is active.
        let export = async {
            for provider in &providers {
                let bytes = cache::read_complete(&*self.client, &provider.path).await?;
                bundle.replace_provider_cache(provider, bytes)?;
            }
            let fresh = self.client.status().await?;
            if !self.client.owns_status(&fresh)
                || fresh.active_generation != status.active_generation
                || fresh.core_pid != status.core_pid
            {
                return Err(unknown());
            }
            Ok::<_, MihomoError>(Arc::new(bundle))
        };
        let bundle = tokio::time::timeout(Duration::from_secs(15), export)
            .await
            .map_err(|_| unknown())??;
        self.confirm_stop().await?;
        state.active.as_mut().ok_or_else(no_snapshot)?.bundle = bundle.clone();
        Ok(bundle)
    }
    pub(crate) async fn restart_accepted(
        &self,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        self.reconcile_locked(&mut state).await?;
        let active = state.active.as_ref().ok_or_else(no_snapshot)?.clone();
        self.confirm_stop().await?;
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(unknown());
        }
        // Stop has changed the runtime. A subsequent rejected Start is not a no-effect restart.
        self.load_snapshot(&active, true, true)
            .await
            .map_err(|_| unknown())
    }
    pub(crate) async fn release_owned(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.released {
            return Ok(());
        }
        state.closing = true;
        self.confirm_stop().await?;
        state.active = None;
        state.candidate = None;
        state.released = true;
        Ok(())
    }
    pub(crate) async fn restore_active(&self) -> MihomoResult<()> {
        let mut state = self.state.lock().await;
        if state.closing || matches!(state.candidate, Some((_, CandidatePhase::Finalizing))) {
            return Err(unknown());
        }
        self.restore_unsaved_locked(&mut state).await
    }
    async fn restore_unsaved_locked(&self, state: &mut RuntimeState) -> MihomoResult<()> {
        if state.candidate.is_none() {
            return Ok(());
        }
        if matches!(state.candidate, Some((_, CandidatePhase::Prepared))) {
            state.candidate = None;
            return Ok(());
        }
        let Some(active) = state.active.clone() else {
            self.confirm_stop().await?;
            state.candidate = None;
            return Err(no_snapshot());
        };
        // A failed Stage may have replaced resources before any controller load.
        // Restart the accepted immutable snapshot rather than trusting partial readback.
        self.load_snapshot(&active, true, true).await?;
        state.candidate = None;
        Ok(())
    }
}

impl<T: RuntimeTransport> PreparedRuntime<T> {
    pub(crate) fn effective_delta(&self) -> Option<&serde_json::Value> {
        self.state.candidate.as_ref()?.0.delta.as_ref()
    }
    pub(crate) async fn apply(mut self, force: bool) -> MihomoResult<AppliedRuntime<T>> {
        let initial = self.state.active.is_none();
        let (candidate, phase) = self.state.candidate.as_mut().ok_or_else(unknown)?;
        *phase = CandidatePhase::Uncertain;
        let candidate = candidate.clone();
        let applied = self
            .owner
            .load_snapshot(&candidate, force, initial || self.recovery)
            .await;
        // Staging can modify assets even if HTTP returns a definite refusal.
        // Retain both snapshots until explicit rollback or shutdown confirms retirement.
        applied?;
        self.state.candidate.as_mut().ok_or_else(unknown)?.1 = CandidatePhase::Applied;
        Ok(AppliedRuntime {
            owner: self.owner,
            state: self.state,
        })
    }
}

impl<T: RuntimeTransport> AppliedRuntime<T> {
    pub(crate) async fn commit(mut self) -> MihomoResult<()> {
        self.state.candidate.as_mut().ok_or_else(unknown)?.1 = CandidatePhase::Finalizing;
        self.owner.confirm_controller().await?;
        self.state.active = self.state.candidate.take().map(|(snapshot, _)| snapshot);
        Ok(())
    }
    pub(crate) async fn rollback(mut self) -> MihomoResult<()> {
        if matches!(self.state.candidate, Some((_, CandidatePhase::Finalizing))) {
            return Err(unknown());
        }
        self.owner.restore_unsaved_locked(&mut self.state).await
    }
}

fn ensure_success(response: NativeHttpResponse) -> MihomoResult<NativeHttpResponse> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    let bounded = &response.body[..response.body.len().min(64 * 1024)];
    let message = serde_json::from_slice::<serde_json::Value>(bounded)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(bounded).into_owned());
    Err(MihomoError::Api {
        status: response.status,
        message,
    })
}
fn partially_applied(error: MihomoError) -> MihomoError {
    if error.mutation_result_unknown() {
        error
    } else {
        MihomoError::RuntimePartiallyApplied(Box::new(error))
    }
}
fn unknown() -> MihomoError {
    MihomoError::RuntimeOutcomeUnknown
}
fn no_snapshot() -> MihomoError {
    MihomoError::Process("No accepted service runtime snapshot".into())
}

#[cfg(test)]
mod tests;
