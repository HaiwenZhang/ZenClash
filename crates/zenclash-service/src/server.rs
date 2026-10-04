use std::{
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{Mutex, Semaphore, watch},
    task::JoinSet,
};

use crate::kernel::Kernel;
use crate::protocol::{
    Request, Response, RuntimeCandidate, RuntimeCandidateKind, RuntimeCandidatePhase,
    RuntimeStatus, ServiceErrorCode, SessionOperation,
};
use crate::runtime::{RetiredRuntime, StagedRuntime, ValidationConfig};
use crate::session::{PeerIdentity, SessionAuthority, SessionError};
use crate::{
    InstalledMetadata, PROTOCOL_VERSION, ProtocolInfo, read_frame, read_metadata, write_frame,
};

const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CONNECTIONS: usize = 16;

struct Stage {
    revision: u64,
    runtime: StagedRuntime,
    validated: bool,
    patch: Option<RuntimePatch>,
}

struct RuntimePatch {
    base_revision: u64,
    requested: serde_json::Value,
    body: serde_json::Value,
    previous: serde_json::Value,
    phase: RuntimeCandidatePhase,
    pid: u32,
    restoring: bool,
}

struct State {
    root: PathBuf,
    installation: InstalledMetadata,
    authority: SessionAuthority,
    owner_lock: Option<crate::maintenance_lock::MaintenanceLock>,
    runtime_recovered: bool,
    revision: u64,
    staged: Option<Stage>,
    active: Option<Stage>,
    kernel: Option<Kernel>,
    closing: bool,
    session_directory: Option<PathBuf>,
    applied_revision: Option<u64>,
    runtime_unknown: bool,
    retired: Option<RetiredRuntime>,
    readback: crate::provider_readback::ProviderReadback,
    #[cfg(test)]
    fixture_root: Option<Arc<crate::installer::OwnedTestRoot>>,
}

impl State {
    fn acquire(&mut self, peer: PeerIdentity) -> Result<Response, ServiceErrorCode> {
        // Existing owners retain the same descriptor across idempotent Acquire.
        let guard = if self.authority.owner().is_none() {
            #[cfg(test)]
            let result = if let Some(root) = &self.fixture_root {
                crate::maintenance_lock::MaintenanceLock::acquire_with(
                    &self.root,
                    true,
                    &|path, directory| root.validate(path, directory),
                )
            } else {
                crate::maintenance_lock::MaintenanceLock::acquire_shared(&self.root)
            };
            #[cfg(not(test))]
            let result = crate::maintenance_lock::MaintenanceLock::acquire_shared(&self.root);
            Some(result.map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    ServiceErrorCode::MaintenancePending
                } else {
                    ServiceErrorCode::Internal
                }
            })?)
        } else {
            None
        };
        // Inspect the journal while maintenance publication is excluded.
        if crate::maintenance_journal::Journal::load(&self.root)
            .map_err(|_| ServiceErrorCode::Internal)?
            .is_some()
        {
            return Err(ServiceErrorCode::MaintenancePending);
        }
        if !self.runtime_recovered {
            self.recover_runtime()
                .map_err(|_| ServiceErrorCode::Internal)?;
            self.runtime_recovered = true;
        }
        let proof = self
            .authority
            .acquire(peer, Instant::now())
            .map_err(session_error)?;
        if let Some(guard) = guard {
            self.owner_lock = Some(guard);
        }
        let next_sequence = self
            .authority
            .next_sequence()
            .ok_or(ServiceErrorCode::Internal)?;
        Ok(Response::Acquired {
            proof,
            next_sequence,
        })
    }

    fn recover_runtime(&self) -> io::Result<()> {
        let root = self.root.join("runtimes");
        self.private_directory(&root)?;
        self.private_directory(&self.root.join("ipc"))?;
        let mut budget = crate::installer::RuntimeCleanupBudget::default();
        for (count, entry) in std::fs::read_dir(root)?.enumerate() {
            if count >= 64 {
                return Err(io::Error::other("stale runtime recovery exceeds budget"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("unknown runtime entry"))?;
            let random = name
                .strip_prefix("stage-")
                .ok_or_else(|| io::Error::other("unknown runtime entry"))?;
            if random.len() != 64 || !random.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(io::Error::other("unknown runtime entry"));
            }
            // The native service manager already contained the previous host's children.
            self.remove_runtime_with_budget(&entry.path(), &mut budget)?;
        }
        Ok(())
    }

    fn readback_preflight(&mut self, revision: u64) -> Result<(), ServiceErrorCode> {
        let snapshot = self.snapshot()?;
        if snapshot.running || snapshot.pid.is_some() {
            return Err(ServiceErrorCode::KernelUnavailable);
        }
        if self.runtime_unknown
            || self.staged.is_some()
            || snapshot.committed_revision != Some(revision)
            || !self
                .active
                .as_ref()
                .is_some_and(|stage| stage.revision == revision)
        {
            return Err(ServiceErrorCode::StaleRevision);
        }
        Ok(())
    }

    async fn begin_provider_cache_read(
        &mut self,
        revision: u64,
        kind: crate::protocol::ProviderKind,
        name: &str,
    ) -> Result<Response, ServiceErrorCode> {
        self.readback_preflight(revision)?;
        let budget = self.readback.reserve(revision, Instant::now())?;
        let source = self
            .active
            .as_ref()
            .ok_or(ServiceErrorCode::StaleRevision)?
            .runtime
            .provider_cache_source(kind, name)
            .map_err(|error| error.code())?;
        let deadline = self.readback.deadline().ok_or(ServiceErrorCode::Internal)?;
        let (result, scanned) =
            tokio::task::spawn_blocking(move || source.snapshot(budget, deadline))
                .await
                .map_err(|_| ServiceErrorCode::Internal)?;
        self.readback.charge(scanned, Instant::now())?;
        if self.authority.expired_owner(Instant::now()).is_some()
            || self
                .authority
                .owner()
                .is_some_and(|peer| !crate::platform::peer_alive(peer))
        {
            self.readback.clear();
            return Err(ServiceErrorCode::Expired);
        }
        self.readback_preflight(revision)?;
        let snapshot = match result.map_err(|error| error.code())? {
            None => crate::protocol::ProviderCacheRead::Absent,
            Some(bytes) => {
                // Compact YAML may expand; scan and transfer share one wave budget.
                self.readback
                    .charge(bytes.len().saturating_sub(scanned), Instant::now())?;
                let prepared = self.readback.publish(revision, bytes, Instant::now())?;
                crate::protocol::ProviderCacheRead::Ready {
                    token: crate::protocol::ProviderCacheToken(prepared.token),
                    len: prepared.len,
                    sha256: prepared.sha256,
                }
            }
        };
        Ok(Response::ProviderCacheRead { snapshot })
    }

    fn session_directory(&mut self) -> io::Result<PathBuf> {
        if let Some(directory) = &self.session_directory {
            return Ok(directory.clone());
        }
        let directory = self
            .root
            .join("runtimes")
            .join(format!("stage-{}", random_hex()?));
        self.private_directory(&directory)?;
        self.private_directory(&directory.join("home"))?;
        self.private_directory(&directory.join("snapshots"))?;
        self.private_directory(&directory.join("assets"))?;
        self.private_directory(&directory.join("configurations"))?;
        self.session_directory = Some(directory.clone());
        Ok(directory)
    }

    fn selected(&self, revision: u64) -> Result<&Stage, ServiceErrorCode> {
        self.staged
            .as_ref()
            .filter(|stage| stage.revision == revision)
            .or_else(|| {
                self.active
                    .as_ref()
                    .filter(|stage| stage.revision == revision)
            })
            .ok_or(ServiceErrorCode::StaleRevision)
    }

    fn binary(&self) -> PathBuf {
        self.root.join(if cfg!(windows) {
            "mihomo.exe"
        } else {
            "mihomo"
        })
    }

    fn validation_config(&self, revision: u64) -> Result<ValidationConfig, ServiceErrorCode> {
        self.selected(revision)?
            .runtime
            .materialize_validation()
            .map_err(|error| error.code())
    }
    fn authorized(&self, peer: &PeerIdentity) -> bool {
        self.installation
            .authorized_users()
            .iter()
            .any(|user| user == peer.user())
    }

    fn accepts_hello_from(&self, peer: &PeerIdentity, maintenance_verified: bool) -> bool {
        self.authorized(peer) || maintenance_verified
    }

    fn accepts_hello(&self, peer: &PeerIdentity) -> bool {
        if self.authorized(peer) {
            return true;
        }
        self.accepts_hello_from(peer, crate::platform::maintenance_identity(peer))
    }

    async fn stop(&mut self) -> io::Result<()> {
        if let Some(kernel) = &mut self.kernel {
            kernel.stop().await?;
        }
        self.applied_revision = None;
        self.runtime_unknown = false;
        Ok(())
    }

    async fn release(&mut self) -> io::Result<()> {
        self.readback.clear();
        self.stop().await?;
        self.kernel = None;
        self.cleanup_retired()?;
        if let Some(stage) = self.staged.take() {
            self.retire(stage);
            self.cleanup_retired()?;
        }
        if let Some(stage) = self.active.take() {
            self.retire(stage);
            self.cleanup_retired()?;
        }
        if let Some(directory) = &self.session_directory {
            self.remove_runtime(directory)?;
            self.session_directory = None;
        }
        if let Some(proof) = self.authority.current_proof().cloned() {
            self.authority
                .release_after_stop(&proof)
                .map_err(io::Error::other)?;
        }
        self.owner_lock = None;
        Ok(())
    }

    fn retire(&mut self, stage: Stage) {
        // Close upload handles before deletion. Retain exact owned roots after
        // a partial failure instead of accumulating untracked directories.
        self.retired = Some(stage.runtime.into_retired());
    }

    fn cleanup_retired(&mut self) -> io::Result<()> {
        let mut budget = crate::installer::RuntimeCleanupBudget::default();
        if let Some(retired) = &self.retired {
            let retained: Vec<_> = self
                .active
                .iter()
                .chain(self.staged.iter())
                .map(|stage| &stage.runtime)
                .collect();
            for directory in retired.directories(&retained) {
                match std::fs::symlink_metadata(&directory) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                    Ok(_) => {}
                }
                self.remove_runtime_with_budget(&directory, &mut budget)?;
            }
            self.retired = None;
        }
        Ok(())
    }

    fn snapshot(&mut self) -> Result<RuntimeStatus, ServiceErrorCode> {
        let mut snapshot = match &mut self.kernel {
            Some(kernel) => kernel.snapshot().map_err(|_| ServiceErrorCode::Internal),
            None => Ok(RuntimeStatus {
                candidate: None,
                running: false,
                pid: None,
                exit_reason: None,
                applied_revision: None,
                committed_revision: None,
            }),
        }?;
        snapshot.applied_revision = self.applied_revision;
        snapshot.committed_revision = self.active.as_ref().map(|stage| stage.revision);
        snapshot.candidate = self.staged.as_ref().map(|stage| RuntimeCandidate {
            revision: stage.revision,
            base_revision: stage.patch.as_ref().map(|patch| patch.base_revision),
            kind: if stage.patch.is_some() {
                RuntimeCandidateKind::Patch
            } else {
                RuntimeCandidateKind::Full
            },
            phase: stage.patch.as_ref().map_or_else(
                || {
                    if self.runtime_unknown {
                        RuntimeCandidatePhase::Uncertain
                    } else if self.applied_revision == Some(stage.revision) {
                        RuntimeCandidatePhase::Applied
                    } else if stage.validated {
                        RuntimeCandidatePhase::Validated
                    } else {
                        RuntimeCandidatePhase::Prepared
                    }
                },
                |patch| patch.phase,
            ),
        });
        Ok(snapshot)
    }

    fn private_directory(&self, directory: &std::path::Path) -> io::Result<()> {
        #[cfg(test)]
        if let Some(root) = &self.fixture_root {
            return root.create_directory(directory);
        }
        crate::platform::private_directory(directory)
    }

    fn remove_runtime(&self, directory: &std::path::Path) -> io::Result<()> {
        self.remove_runtime_with_budget(
            directory,
            &mut crate::installer::RuntimeCleanupBudget::default(),
        )
    }

    fn remove_runtime_with_budget(
        &self,
        directory: &std::path::Path,
        budget: &mut crate::installer::RuntimeCleanupBudget,
    ) -> io::Result<()> {
        #[cfg(test)]
        if let Some(root) = &self.fixture_root {
            return root.remove_with_budget(directory, budget);
        }
        crate::platform::validate_protected_path(directory, true)?;
        crate::installer::remove_private_runtime_with_budget(directory, 0, &mut 0, budget)
    }

    fn running_pid(&mut self) -> Result<u32, ServiceErrorCode> {
        let snapshot = self
            .kernel
            .as_mut()
            .ok_or(ServiceErrorCode::KernelUnavailable)?
            .snapshot()
            .map_err(|_| ServiceErrorCode::KernelUnavailable)?;
        if !snapshot.running {
            return Err(ServiceErrorCode::KernelUnavailable);
        }
        snapshot.pid.ok_or(ServiceErrorCode::KernelUnavailable)
    }

    async fn prepare_patch(
        &mut self,
        base_revision: u64,
        body: serde_json::Value,
    ) -> Result<Response, ServiceErrorCode> {
        let requested = crate::api::canonical_runtime_patch(&body)?;
        if let Some(candidate) = &self.staged {
            if candidate.patch.as_ref().is_some_and(|patch| {
                patch.base_revision == base_revision && patch.requested == requested
            }) {
                return Ok(Response::RuntimePatchPrepared {
                    prepared: crate::protocol::PreparedRuntimePatch {
                        revision: candidate.revision,
                        effective_patch: candidate
                            .patch
                            .as_ref()
                            .ok_or(ServiceErrorCode::Internal)?
                            .body
                            .clone(),
                    },
                });
            }
            return Err(ServiceErrorCode::InvalidRequest);
        }
        if self.runtime_unknown {
            return Err(ServiceErrorCode::OutcomeUnknown);
        }
        if self.applied_revision != Some(base_revision)
            || !self
                .active
                .as_ref()
                .is_some_and(|active| active.revision == base_revision)
        {
            return Err(ServiceErrorCode::StaleRevision);
        }
        self.cleanup_retired()
            .map_err(|_| ServiceErrorCode::Internal)?;
        let pid = self.running_pid()?;
        let actual = self
            .kernel
            .as_ref()
            .ok_or(ServiceErrorCode::KernelUnavailable)?
            .runtime_config()
            .await?;
        if self.running_pid()? != pid {
            return Err(ServiceErrorCode::KernelUnavailable);
        }
        let body = crate::api::complete_runtime_patch(&requested, &actual)?;
        let previous = crate::api::affected_values(&actual, &body)?;
        let previous = crate::api::canonical_kernel_runtime_patch(&previous)
            .map_err(|_| ServiceErrorCode::KernelFailed)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(ServiceErrorCode::Internal)?;
        let session = self
            .session_directory()
            .map_err(|_| ServiceErrorCode::Internal)?;
        let configuration = session
            .join("configurations")
            .join(format!("revision-{revision}"));
        self.retired = Some(
            self.active
                .as_ref()
                .ok_or(ServiceErrorCode::StaleRevision)?
                .runtime
                .retirement_for_configuration(configuration.clone()),
        );
        self.private_directory(&configuration)
            .map_err(|_| ServiceErrorCode::Internal)?;
        let runtime = self
            .active
            .as_ref()
            .ok_or(ServiceErrorCode::StaleRevision)?
            .runtime
            .fork_patch(configuration, &body)
            .map_err(|error| error.code())?;
        self.retired = None;
        self.staged = Some(Stage {
            revision,
            runtime,
            validated: true,
            patch: Some(RuntimePatch {
                base_revision,
                requested,
                body,
                previous,
                phase: RuntimeCandidatePhase::Prepared,
                pid,
                restoring: false,
            }),
        });
        self.revision = revision;
        Ok(Response::RuntimePatchPrepared {
            prepared: crate::protocol::PreparedRuntimePatch {
                revision,
                effective_patch: self
                    .staged
                    .as_ref()
                    .and_then(|stage| stage.patch.as_ref())
                    .ok_or(ServiceErrorCode::Internal)?
                    .body
                    .clone(),
            },
        })
    }

    async fn reconcile_patch(&mut self, revision: u64) -> Result<(), ServiceErrorCode> {
        let candidate = self
            .staged
            .as_ref()
            .filter(|candidate| candidate.revision == revision)
            .ok_or(ServiceErrorCode::StaleRevision)?;
        let patch = candidate
            .patch
            .as_ref()
            .ok_or(ServiceErrorCode::InvalidRequest)?;
        let (body, previous, pid, base, restoring) = (
            patch.body.clone(),
            patch.previous.clone(),
            patch.pid,
            patch.base_revision,
            patch.restoring,
        );
        if self.running_pid().ok() != Some(pid) {
            return Err(ServiceErrorCode::OutcomeUnknown);
        }
        let actual = self
            .kernel
            .as_ref()
            .ok_or(ServiceErrorCode::OutcomeUnknown)?
            .runtime_config()
            .await
            .map_err(|_| ServiceErrorCode::OutcomeUnknown)?;
        if self.running_pid().ok() != Some(pid) {
            return Err(ServiceErrorCode::OutcomeUnknown);
        }
        let selected = crate::api::affected_values(&actual, &body)
            .map_err(|_| ServiceErrorCode::OutcomeUnknown)?;
        let phase = if selected == previous && (restoring || body != previous) {
            RuntimeCandidatePhase::Prepared
        } else if selected == body {
            RuntimeCandidatePhase::Applied
        } else {
            RuntimeCandidatePhase::Uncertain
        };
        self.staged
            .as_mut()
            .and_then(|stage| stage.patch.as_mut())
            .ok_or(ServiceErrorCode::StaleRevision)?
            .phase = phase;
        self.runtime_unknown = phase == RuntimeCandidatePhase::Uncertain;
        self.applied_revision = match phase {
            RuntimeCandidatePhase::Prepared => Some(base),
            RuntimeCandidatePhase::Applied => Some(revision),
            _ => None,
        };
        if self.runtime_unknown {
            Err(ServiceErrorCode::OutcomeUnknown)
        } else {
            Ok(())
        }
    }

    async fn operate_patch(
        &mut self,
        revision: u64,
        restoring: bool,
    ) -> Result<Response, ServiceErrorCode> {
        let candidate = self
            .staged
            .as_ref()
            .filter(|candidate| candidate.revision == revision)
            .ok_or(ServiceErrorCode::StaleRevision)?;
        let patch = candidate
            .patch
            .as_ref()
            .ok_or(ServiceErrorCode::InvalidRequest)?;
        let (pid, mut phase, base, body) = (
            patch.pid,
            patch.phase,
            patch.base_revision,
            if restoring {
                patch.previous.clone()
            } else {
                patch.body.clone()
            },
        );
        if !self
            .active
            .as_ref()
            .is_some_and(|active| active.revision == base)
        {
            return Err(ServiceErrorCode::StaleRevision);
        }
        if restoring {
            let snapshot = self.snapshot()?;
            if !snapshot.running && snapshot.pid.is_none() {
                // Stop has been confirmed; no controller PATCH or base Commit is needed.
                // Keep the candidate and its cleanup owner until deletion succeeds.
                self.cleanup_retired()
                    .map_err(|_| ServiceErrorCode::Internal)?;
                let configuration = self
                    .session_directory
                    .as_ref()
                    .ok_or(ServiceErrorCode::Internal)?
                    .join("configurations")
                    .join(format!("revision-{revision}"));
                self.retired = Some(
                    self.staged
                        .as_ref()
                        .ok_or(ServiceErrorCode::StaleRevision)?
                        .runtime
                        .retirement_for_configuration(configuration),
                );
                self.cleanup_retired()
                    .map_err(|_| ServiceErrorCode::Internal)?;
                self.staged = None;
                self.applied_revision = None;
                self.runtime_unknown = false;
                return Ok(Response::Ok);
            }
        }
        if self.running_pid().ok() != Some(pid) {
            return Err(if phase == RuntimeCandidatePhase::Uncertain {
                ServiceErrorCode::OutcomeUnknown
            } else {
                ServiceErrorCode::KernelUnavailable
            });
        }
        if phase == RuntimeCandidatePhase::Uncertain {
            self.reconcile_patch(revision).await?;
            phase = self
                .staged
                .as_ref()
                .and_then(|stage| stage.patch.as_ref())
                .ok_or(ServiceErrorCode::StaleRevision)?
                .phase;
            match (restoring, phase) {
                (false, RuntimeCandidatePhase::Applied)
                | (true, RuntimeCandidatePhase::Prepared) => return Ok(Response::Ok),
                (true, RuntimeCandidatePhase::Applied) => {}
                // A confirmed old state permits an explicit later attempt,
                // but never automatically resends an uncertain application.
                _ => return Err(ServiceErrorCode::KernelFailed),
            }
        }
        if (!restoring && phase == RuntimeCandidatePhase::Applied)
            || (restoring && phase == RuntimeCandidatePhase::Prepared)
        {
            return Ok(Response::Ok);
        }
        self.staged
            .as_mut()
            .and_then(|stage| stage.patch.as_mut())
            .ok_or(ServiceErrorCode::StaleRevision)?
            .restoring = restoring;
        let result = self
            .kernel
            .as_ref()
            .ok_or(ServiceErrorCode::KernelUnavailable)?
            .patch_runtime(&body)
            .await;
        if let Err(error) = &result
            && *error != ServiceErrorCode::OutcomeUnknown
        {
            return result.map(|()| Response::Ok);
        }
        // Once any mutation might have been sent, failed GETs cannot turn it
        // into a prepared/zero-send candidate, including after stop or PID loss.
        self.staged
            .as_mut()
            .and_then(|stage| stage.patch.as_mut())
            .ok_or(ServiceErrorCode::StaleRevision)?
            .phase = RuntimeCandidatePhase::Uncertain;
        self.runtime_unknown = true;
        self.applied_revision = None;
        self.reconcile_patch(revision).await?;
        let phase = self
            .staged
            .as_ref()
            .and_then(|stage| stage.patch.as_ref())
            .ok_or(ServiceErrorCode::StaleRevision)?
            .phase;
        match (restoring, phase) {
            (false, RuntimeCandidatePhase::Applied) | (true, RuntimeCandidatePhase::Prepared) => {
                Ok(Response::Ok)
            }
            _ => Err(ServiceErrorCode::KernelFailed),
        }
    }

    async fn operate(&mut self, operation: SessionOperation) -> Result<Response, ServiceErrorCode> {
        let revision = self.active.as_ref().map_or(0, |active| active.revision);
        self.readback.expire(revision, Instant::now());
        if !matches!(
            &operation,
            SessionOperation::BeginProviderCacheRead { .. }
                | SessionOperation::ReadProviderCache { .. }
                | SessionOperation::FinishProviderCacheRead { .. }
                | SessionOperation::Status {}
                | SessionOperation::Logs { .. }
        ) {
            self.readback.invalidate();
        }
        if !matches!(
            &operation,
            SessionOperation::Status {} | SessionOperation::Stop {} | SessionOperation::Release {}
        ) && crate::maintenance_journal::Journal::load(&self.root)
            .map_err(|_| ServiceErrorCode::Internal)?
            .is_some()
        {
            return Err(ServiceErrorCode::MaintenancePending);
        }
        match operation {
            SessionOperation::BeginProviderCacheRead {
                revision,
                kind,
                name,
            } => self.begin_provider_cache_read(revision, kind, &name).await,
            SessionOperation::ReadProviderCache { token, offset } => {
                self.readback_preflight(revision)?;
                let bytes = self
                    .readback
                    .read(revision, &token.0, offset, Instant::now())?;
                Ok(Response::ProviderCacheChunk {
                    chunk: crate::protocol::ProviderCacheChunk {
                        offset,
                        bytes,
                        finished: self.readback.finished(),
                    },
                })
            }
            SessionOperation::FinishProviderCacheRead { token } => {
                self.readback.finish(&token.0)?;
                Ok(Response::Ok)
            }
            SessionOperation::Status {} => {
                if let Some(revision) =
                    self.staged
                        .as_ref()
                        .filter(|stage| {
                            stage.patch.as_ref().is_some_and(|patch| {
                                patch.phase == RuntimeCandidatePhase::Uncertain
                            })
                        })
                        .map(|stage| stage.revision)
                {
                    let _ = self.reconcile_patch(revision).await;
                }
                Ok(Response::Status {
                    snapshot: self.snapshot()?,
                })
            }
            SessionOperation::PrepareRuntimePatch {
                base_revision,
                patch,
            } => self.prepare_patch(base_revision, patch).await,
            SessionOperation::ApplyRuntimePatch { revision } => {
                self.operate_patch(revision, false).await
            }
            SessionOperation::RestoreRuntimePatch { revision } => {
                self.operate_patch(revision, true).await
            }
            SessionOperation::Release {} => {
                self.release()
                    .await
                    .map_err(|_| ServiceErrorCode::Internal)?;
                Ok(Response::Ok)
            }
            SessionOperation::Stop {} => {
                self.stop()
                    .await
                    .map_err(|_| ServiceErrorCode::KernelFailed)?;
                Ok(Response::Ok)
            }
            SessionOperation::Stage { config } => {
                if self.runtime_unknown
                    || self
                        .staged
                        .as_ref()
                        .is_some_and(|stage| stage.patch.is_some())
                    || self
                        .staged
                        .as_ref()
                        .is_some_and(|stage| Some(stage.revision) == self.applied_revision)
                {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                self.cleanup_retired()
                    .map_err(|_| ServiceErrorCode::Internal)?;
                if let Some(stage) = self.staged.take() {
                    self.retire(stage);
                    self.cleanup_retired()
                        .map_err(|_| ServiceErrorCode::Internal)?;
                }
                let revision = self
                    .revision
                    .checked_add(1)
                    .ok_or(ServiceErrorCode::Internal)?;
                let session = self
                    .session_directory()
                    .map_err(|_| ServiceErrorCode::Internal)?;
                let directory = session
                    .join("snapshots")
                    .join(format!("revision-{revision}"));
                let configuration = session
                    .join("configurations")
                    .join(format!("revision-{revision}"));
                let mut runtime = StagedRuntime::new_with_configuration(
                    directory.clone(),
                    configuration.clone(),
                    &config,
                )
                .map_err(|error| error.code())?;
                if let Some(active) = &self.active {
                    runtime.inherit_resources(&active.runtime);
                }
                self.retired = Some(runtime.retirement_for_configuration(configuration.clone()));
                self.private_directory(&directory)
                    .map_err(|_| ServiceErrorCode::Internal)?;
                self.private_directory(&configuration)
                    .map_err(|_| ServiceErrorCode::Internal)?;
                self.retired = None;
                self.staged = Some(Stage {
                    revision,
                    runtime,
                    validated: false,
                    patch: None,
                });
                self.revision = revision;
                Ok(Response::Staged { revision })
            }
            SessionOperation::UploadAsset {
                path,
                offset,
                bytes,
                finished,
            } => {
                let staged = self.staged.as_mut().ok_or(ServiceErrorCode::NotStaged)?;
                if self.runtime_unknown
                    || self.applied_revision == Some(staged.revision)
                    || staged
                        .runtime
                        .has_materialized()
                        .map_err(|error| error.code())?
                {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                staged.validated = false;
                staged
                    .runtime
                    .upload(&path, offset, &bytes, finished)
                    .map_err(|error| error.code())?;
                Ok(Response::Ok)
            }
            SessionOperation::Start { revision } => {
                if self
                    .staged
                    .as_ref()
                    .is_some_and(|stage| stage.patch.is_some())
                {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                if self.snapshot()?.running {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                let selected = self.selected(revision)?;
                if !selected.validated {
                    return Err(ServiceErrorCode::InvalidConfiguration);
                }
                let random = random_hex().map_err(|_| ServiceErrorCode::Internal)?;
                #[cfg(windows)]
                let controller = PathBuf::from(format!(r"\\.\pipe\ZenClash.Kernel.{random}"));
                #[cfg(unix)]
                let controller = self
                    .root
                    .join("ipc")
                    .join(format!("kernel-{}.sock", &random[..16]));
                let secret = random_hex().map_err(|_| ServiceErrorCode::Internal)?;
                let controller_text = controller.to_str().ok_or(ServiceErrorCode::Internal)?;
                let config = selected
                    .runtime
                    .materialize(controller_text, &secret)
                    .map_err(|error| error.code())?;
                let binary = self.binary();
                verify_binary(&binary, self.installation.core_sha256())
                    .map_err(|_| ServiceErrorCode::KernelUnavailable)?;
                let home = self
                    .session_directory
                    .as_ref()
                    .ok_or(ServiceErrorCode::Internal)?
                    .join("home");
                selected
                    .runtime
                    .install_geodata(&home)
                    .map_err(|error| error.code())?;
                let kernel = Kernel::start(&binary, &config, &home, controller, secret)
                    .await
                    .map_err(|_| ServiceErrorCode::KernelFailed)?;
                // A new confirmed writer lifecycle permits a new cache wave.
                // Rejected or failed starts keep the previous cumulative budget.
                self.readback.clear();
                self.kernel = Some(kernel);
                self.applied_revision = Some(revision);
                self.runtime_unknown = false;
                Ok(Response::Ok)
            }
            SessionOperation::Validate { revision } => {
                let selected = self.selected(revision)?;
                if selected.patch.is_some() {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                let home = self
                    .session_directory
                    .as_ref()
                    .ok_or(ServiceErrorCode::Internal)?
                    .join("validation");
                crate::platform::private_directory(&home)
                    .map_err(|_| ServiceErrorCode::Internal)?;
                selected
                    .runtime
                    .install_geodata(&home)
                    .map_err(|error| error.code())?;
                let config = self.validation_config(revision)?;
                let binary = self.binary();
                let result = async {
                    verify_binary(&binary, self.installation.core_sha256())
                        .map_err(|_| ServiceErrorCode::KernelUnavailable)?;
                    Kernel::validate(&binary, config.path(), &home)
                        .await
                        .map_err(|_| ServiceErrorCode::InvalidConfiguration)
                }
                .await;
                config.cleanup().map_err(|error| error.code())?;
                result?;
                if let Some(stage) = self
                    .staged
                    .as_mut()
                    .filter(|stage| stage.revision == revision)
                {
                    stage.validated = true;
                }
                if let Some(stage) = self
                    .active
                    .as_mut()
                    .filter(|stage| stage.revision == revision)
                {
                    stage.validated = true;
                }
                Ok(Response::Ok)
            }
            SessionOperation::Reload { revision, force } => {
                if self
                    .staged
                    .as_ref()
                    .is_some_and(|stage| stage.patch.is_some())
                {
                    return Err(ServiceErrorCode::InvalidRequest);
                }
                let selected = self.selected(revision)?;
                if !selected.validated {
                    return Err(ServiceErrorCode::InvalidConfiguration);
                }
                let home = self
                    .session_directory
                    .as_ref()
                    .ok_or(ServiceErrorCode::Internal)?
                    .join("home");
                selected
                    .runtime
                    .install_geodata(&home)
                    .map_err(|error| error.code())?;
                let kernel = self
                    .kernel
                    .as_ref()
                    .ok_or(ServiceErrorCode::KernelUnavailable)?;
                let result = kernel.reload(&selected.runtime, force).await;
                self.observe_reload(revision, result)?;
                Ok(Response::Ok)
            }
            SessionOperation::CommitRuntime { revision } => {
                if self.runtime_unknown || self.applied_revision != Some(revision) {
                    return Err(ServiceErrorCode::StaleRevision);
                }
                self.selected(revision)?;
                if self.staged.as_ref().is_some_and(|stage| {
                    stage.revision == revision
                        && stage
                            .patch
                            .as_ref()
                            .is_some_and(|patch| patch.phase != RuntimeCandidatePhase::Applied)
                }) {
                    return Err(ServiceErrorCode::StaleRevision);
                }
                if self
                    .staged
                    .as_ref()
                    .is_some_and(|stage| stage.revision == revision)
                {
                    if let Some(previous) = self.active.take() {
                        self.retire(previous);
                    }
                    self.active = self.staged.take();
                } else {
                    if let Some(candidate) = self.staged.take() {
                        self.retire(candidate);
                    }
                }
                // Cleanup cannot undo an already accepted business commit.
                // A failed cleanup blocks further staging until retried.
                let _ = self.cleanup_retired();
                self.readback.expire(revision, Instant::now());
                Ok(Response::Ok)
            }
            SessionOperation::Logs { cursor } => {
                let (cursor, lines) = self
                    .kernel
                    .as_ref()
                    .map(|kernel| kernel.logs(cursor))
                    .unwrap_or((cursor, Vec::new()));
                Ok(Response::Logs { cursor, lines })
            }
            SessionOperation::Api { request } => {
                if !self.snapshot()?.running {
                    return Err(ServiceErrorCode::KernelUnavailable);
                }
                let kernel = self
                    .kernel
                    .as_ref()
                    .ok_or(ServiceErrorCode::KernelUnavailable)?;
                Ok(Response::Api {
                    response: kernel.api(&request).await?,
                })
            }
            // Subscription connections are admitted and dispatched separately.
            SessionOperation::Subscribe { .. } | SessionOperation::SubscribeLogs { .. } => {
                Err(ServiceErrorCode::InvalidRequest)
            }
        }
    }

    fn observe_reload(
        &mut self,
        revision: u64,
        result: Result<(), ServiceErrorCode>,
    ) -> Result<(), ServiceErrorCode> {
        match result {
            Ok(()) => {
                self.applied_revision = Some(revision);
                self.runtime_unknown = false;
                Ok(())
            }
            Err(ServiceErrorCode::OutcomeUnknown) => {
                self.applied_revision = None;
                self.runtime_unknown = true;
                Err(ServiceErrorCode::OutcomeUnknown)
            }
            Err(error) => Err(error),
        }
    }
}

fn random_hex() -> io::Result<String> {
    use std::fmt::Write;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("random source unavailable"))?;
    let mut value = String::with_capacity(64);
    for byte in random {
        write!(&mut value, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(value)
}

fn verify_binary(path: &std::path::Path, expected: &str) -> io::Result<()> {
    use std::io::Read;
    crate::platform::validate_protected_path(path, false)?;
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        return Err(io::Error::other("approved kernel exceeds budget"));
    }
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected.to_ascii_lowercase() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "approved kernel digest changed",
        ));
    }
    Ok(())
}

fn session_error(error: SessionError) -> ServiceErrorCode {
    match error {
        SessionError::Occupied => ServiceErrorCode::Occupied,
        SessionError::Unauthorized => ServiceErrorCode::Unauthorized,
        SessionError::Expired => ServiceErrorCode::Expired,
        SessionError::Replay => ServiceErrorCode::Replay,
        SessionError::Random | SessionError::GenerationExhausted => ServiceErrorCode::Internal,
    }
}

async fn connection<T: AsyncRead + AsyncWrite + Unpin + Send>(
    mut stream: T,
    peer: PeerIdentity,
    shared_state: Arc<Mutex<State>>,
) -> io::Result<()> {
    let first: Request = read_frame(&mut stream, FRAME_TIMEOUT)
        .await
        .map_err(io::Error::other)?;
    let compatible = matches!(
        first,
        Request::Hello {
            protocol_version: PROTOCOL_VERSION
        }
    );
    let response = if compatible {
        Response::Hello {
            info: ProtocolInfo::current(),
        }
    } else {
        Response::Error {
            code: ServiceErrorCode::Incompatible,
        }
    };
    write_frame(&mut stream, &response, FRAME_TIMEOUT)
        .await
        .map_err(io::Error::other)?;
    if !compatible {
        return Ok(());
    }
    loop {
        let request: Request = read_frame(&mut stream, FRAME_TIMEOUT)
            .await
            .map_err(io::Error::other)?;
        if matches!(
            &request,
            Request::Session {
                operation: SessionOperation::BeginProviderCacheRead { .. },
                ..
            }
        ) {
            let Request::Session {
                proof,
                sequence,
                operation,
            } = request
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid readback request",
                ));
            };
            // Queue cancellation has no effects. Once admitted, the owned completion
            // retains the same state gate until its filesystem worker has released its pins.
            let mut state = shared_state.clone().lock_owned().await;
            let admitted = if state.closing {
                Err(ServiceErrorCode::Expired)
            } else if !state.authorized(&peer) || !crate::platform::peer_alive(&peer) {
                Err(ServiceErrorCode::Unauthorized)
            } else {
                state
                    .authority
                    .admit(&peer, &proof, sequence, Instant::now())
                    .map_err(session_error)
            };
            let result = match admitted {
                Ok(()) => tokio::spawn(async move { state.operate(operation).await })
                    .await
                    .unwrap_or(Err(ServiceErrorCode::Internal)),
                Err(code) => Err(code),
            };
            let response = result.unwrap_or_else(|code| Response::Error { code });
            write_frame(&mut stream, &response, FRAME_TIMEOUT)
                .await
                .map_err(io::Error::other)?;
            continue;
        }
        let mut state = shared_state.lock().await;
        let mut subscription = None;
        let result = if state.closing {
            Err(ServiceErrorCode::Expired)
        } else if !state.authorized(&peer) || !crate::platform::peer_alive(&peer) {
            Err(ServiceErrorCode::Unauthorized)
        } else {
            match request {
                Request::Acquire {} => state.acquire(peer.clone()),
                Request::Session {
                    proof,
                    sequence,
                    operation,
                } => {
                    match state
                        .authority
                        .admit(&peer, &proof, sequence, Instant::now())
                    {
                        Ok(()) => {
                            if matches!(
                                &operation,
                                SessionOperation::Subscribe { .. }
                                    | SessionOperation::SubscribeLogs { .. }
                            ) {
                                if matches!(
                                    &operation,
                                    SessionOperation::Subscribe {
                                        stream: crate::protocol::StreamKind::Logs
                                    }
                                ) {
                                    Err(ServiceErrorCode::InvalidRequest)
                                } else if !state.snapshot().is_ok_and(|snapshot| snapshot.running) {
                                    Err(ServiceErrorCode::KernelUnavailable)
                                } else if let Some(kernel) = &state.kernel {
                                    let events = match operation {
                                        SessionOperation::Subscribe { stream } => {
                                            kernel.subscribe(stream).await
                                        }
                                        SessionOperation::SubscribeLogs { options } => {
                                            kernel.subscribe_logs(options).await
                                        }
                                        _ => Err(io::Error::new(
                                            io::ErrorKind::InvalidInput,
                                            "invalid subscription operation",
                                        )),
                                    };
                                    match events {
                                        Ok(events) => {
                                            subscription = Some((proof, events));
                                            Ok(Response::Ok)
                                        }
                                        Err(_) => Err(ServiceErrorCode::KernelUnavailable),
                                    }
                                } else {
                                    Err(ServiceErrorCode::KernelUnavailable)
                                }
                            } else {
                                state.operate(operation).await
                            }
                        }
                        Err(error) => Err(session_error(error)),
                    }
                }
                Request::Hello { .. } => Err(ServiceErrorCode::InvalidRequest),
            }
        };
        drop(state);
        let response = result.unwrap_or_else(|code| Response::Error { code });
        write_frame(&mut stream, &response, FRAME_TIMEOUT)
            .await
            .map_err(io::Error::other)?;
        if let Some((proof, events)) = subscription {
            let pid = events.pid;
            let events = futures_util::stream::try_unfold(events, |mut events| async move {
                events
                    .next()
                    .await
                    .map(|event| event.map(|event| (event, events)))
            });
            return forward_events(&mut stream, events, || async {
                let mut state = shared_state.lock().await;
                if state.closing || !state.authority.stream_valid(&peer, &proof, Instant::now()) {
                    return Err(ServiceErrorCode::Expired);
                }
                if !crate::platform::peer_alive(&peer) {
                    return Err(ServiceErrorCode::Expired);
                }
                let snapshot = state.snapshot()?;
                if !snapshot.running || snapshot.pid != Some(pid) {
                    return Err(ServiceErrorCode::KernelUnavailable);
                }
                Ok(())
            })
            .await;
        }
    }
}

async fn forward_events<T, S, V, F>(stream: &mut T, events: S, valid: V) -> io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Send,
    S: Stream<Item = io::Result<serde_json::Value>>,
    V: Fn() -> F,
    F: std::future::Future<Output = Result<(), ServiceErrorCode>>,
{
    futures_util::pin_mut!(events);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    loop {
        let response = tokio::select! {
            biased;
            event = events.next() => {
                let Some(event) = event else { return Ok(()); };
                let data = event?;
                match valid().await {
                    Ok(()) => Response::Stream { data },
                    Err(code) => Response::Error { code },
                }
            }
            incoming = stream.read_u8() => {
                return match incoming {
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
                    Err(error) => Err(error),
                    Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "subscription cannot accept requests")),
                };
            }
            _ = heartbeat.tick() => {
                match valid().await { Ok(()) => continue, Err(code) => Response::Error { code } }
            }
        };
        let terminal = matches!(response, Response::Error { .. });
        write_frame(stream, &response, Duration::from_secs(5))
            .await
            .map_err(io::Error::other)?;
        if terminal {
            return Ok(());
        }
    }
}

pub(crate) async fn run(mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
    let root = crate::platform::root_directory()?;
    crate::platform::validate_protected_path(&root, true)?;
    let metadata_path = root.join("install.json");
    crate::platform::validate_protected_path(&metadata_path, false)?;
    let installation = read_metadata(&metadata_path).map_err(io::Error::other)?;
    if installation.protocol_version() != PROTOCOL_VERSION {
        return Err(io::Error::other("installed protocol incompatible"));
    }
    let state = Arc::new(Mutex::new(State {
        root,
        installation,
        authority: SessionAuthority::default(),
        owner_lock: None,
        runtime_recovered: false,
        revision: 0,
        staged: None,
        active: None,
        kernel: None,
        closing: false,
        session_directory: None,
        applied_revision: None,
        runtime_unknown: false,
        retired: None,
        readback: Default::default(),
        #[cfg(test)]
        fixture_root: None,
    }));
    let listener = crate::platform::Listener::bind()?;
    #[cfg(windows)]
    let mut listener = listener;
    #[cfg(windows)]
    crate::platform::report_ready();
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tasks = JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    let result = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break Ok(()); }
            }
            _ = heartbeat.tick() => {
                let mut state = state.lock().await;
                let revision = state.active.as_ref().map_or(0, |active| active.revision);
                state.readback.expire(revision, Instant::now());
                let stale = state.authority.expired_owner(Instant::now()).is_some()
                    || state.authority.owner().is_some_and(|peer| !crate::platform::peer_alive(peer));
                if stale && let Err(error) = state.release().await { break Err(error); }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        if !state.lock().await.accepts_hello(&peer) { continue; }
                        if let Ok(permit) = connections.clone().try_acquire_owned() {
                            let state = state.clone();
                            tasks.spawn(async move { let _permit = permit; connection(stream, peer, state).await });
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (),
                    Err(error) => break Err(error),
                }
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => (),
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let mut state = state.lock().await;
    state.closing = true;
    state.release().await?;
    result
}

#[cfg(test)]
#[path = "server_maintenance_tests.rs"]
mod maintenance_tests;

#[cfg(test)]
#[path = "server_patch_tests.rs"]
mod patch_tests;

#[cfg(test)]
#[path = "server_readback_tests.rs"]
mod readback_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) fn state_for_test(root: PathBuf) -> State {
        State {
            root,
            installation: InstalledMetadata::new(
                "aa".repeat(32),
                "bb".repeat(32),
                "test-user".into(),
            )
            .unwrap(),
            authority: SessionAuthority::default(),
            owner_lock: None,
            runtime_recovered: true,
            revision: 2,
            staged: None,
            active: None,
            kernel: None,
            closing: false,
            session_directory: None,
            applied_revision: Some(1),
            runtime_unknown: false,
            retired: None,
            readback: Default::default(),
            fixture_root: None,
        }
    }

    #[tokio::test]
    async fn staging_a_same_url_candidate_copies_cache_and_release_removes_both_groups() {
        let fixture = Arc::new(crate::installer::OwnedTestRoot::create().unwrap());
        let root = fixture.path().to_path_buf();
        let session = root.join("session");
        for path in [
            &session,
            &session.join("snapshots"),
            &session.join("assets"),
            &session.join("configurations"),
            &session.join("snapshots/revision-1"),
            &session.join("configurations/revision-1"),
        ] {
            fixture.create_directory(path).unwrap();
        }
        let yaml = "rule-providers:\n  r:\n    type: http\n    behavior: classical\n    url: https://example.invalid\n";
        let runtime = StagedRuntime::new_with_configuration(
            session.join("snapshots/revision-1"),
            session.join("configurations/revision-1"),
            yaml,
        )
        .unwrap();
        let config = runtime.materialize("controller", "secret").unwrap();
        let value: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(config).unwrap()).unwrap();
        let accepted_cache = PathBuf::from(value["rule-providers"]["r"]["path"].as_str().unwrap());
        std::fs::write(&accepted_cache, b"accepted latest cache").unwrap();
        let mut state = state_for_test(root.clone());
        state.fixture_root = Some(fixture.clone());
        state.session_directory = Some(session.clone());
        state.active = Some(Stage {
            revision: 1,
            runtime,
            validated: true,
            patch: None,
        });
        let Response::Staged { revision } = state
            .operate(SessionOperation::Stage {
                config: yaml.into(),
            })
            .await
            .unwrap()
        else {
            panic!("expected a stage");
        };
        state
            .validation_config(revision)
            .unwrap()
            .cleanup()
            .unwrap();
        let candidate = state.staged.as_ref().unwrap().runtime.prepared_directory();
        let relative = accepted_cache
            .strip_prefix(state.active.as_ref().unwrap().runtime.prepared_directory())
            .unwrap();
        assert_eq!(
            std::fs::read(candidate.join(relative)).unwrap(),
            b"accepted latest cache"
        );
        assert_eq!(
            std::fs::read(accepted_cache).unwrap(),
            b"accepted latest cache"
        );
        state.release().await.unwrap();
        assert!(!session.exists());
    }

    #[tokio::test]
    async fn committing_a_config_only_candidate_preserves_shared_resource_roots() {
        let fixture = Arc::new(crate::installer::OwnedTestRoot::create().unwrap());
        let root = fixture.path().to_path_buf();
        let input = root.join("snapshots/revision-1");
        let configurations = root.join("configurations");
        fixture.create_directory(&root.join("snapshots")).unwrap();
        fixture.create_directory(&root.join("assets")).unwrap();
        fixture.create_directory(&configurations).unwrap();
        fixture.create_directory(&input).unwrap();
        let mut state = state_for_test(root.clone());
        state.fixture_root = Some(fixture.clone());
        for revision in [1, 2] {
            let configuration = configurations.join(format!("revision-{revision}"));
            fixture.create_directory(&configuration).unwrap();
            let mut runtime = StagedRuntime::new_with_configuration(
                input.clone(),
                configuration,
                "mode: rule\nclient-auth-cert: assets/cert.pem\n",
            )
            .unwrap();
            if revision == 1 {
                runtime
                    .upload("assets/cert.pem", 0, b"held certificate bytes", true)
                    .unwrap();
                runtime
                    .materialize("fixed-controller", "fixed-secret")
                    .unwrap();
            }
            let stage = Stage {
                revision,
                runtime,
                validated: true,
                patch: None,
            };
            if revision == 1 {
                state.active = Some(stage);
            } else {
                state.staged = Some(stage);
            }
        }
        let asset = root.join("assets/revision-1/assets/cert.pem");
        assert_eq!(std::fs::read(&asset).unwrap(), b"held certificate bytes");
        state.applied_revision = Some(2);
        assert!(matches!(
            state
                .operate(SessionOperation::CommitRuntime { revision: 2 })
                .await,
            Ok(Response::Ok)
        ));
        assert_eq!(std::fs::read(&asset).unwrap(), b"held certificate bytes");
        assert!(input.join("assets/cert.pem").is_file());
        assert!(!configurations.join("revision-1").exists());
        assert!(configurations.join("revision-2").is_dir());
        state.release().await.unwrap();
        drop(state);
        for entry in std::fs::read_dir(&root).unwrap() {
            fixture.remove(&entry.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(&root).unwrap();
    }

    #[tokio::test]
    async fn stopped_and_failed_start_revisions_cannot_accept_more_asset_uploads() {
        for failed_start in [false, true] {
            let root = std::env::temp_dir()
                .join(format!("zenclash-frozen-stage-{}", random_hex().unwrap()));
            std::fs::create_dir(&root).unwrap();
            let mut state = state_for_test(root.clone());
            state.staged = Some(Stage {
                revision: 2,
                runtime: StagedRuntime::new(root.clone(), "mode: rule").unwrap(),
                validated: true,
                patch: None,
            });
            if failed_start {
                assert!(matches!(
                    state.operate(SessionOperation::Start { revision: 2 }).await,
                    Err(ServiceErrorCode::KernelUnavailable)
                ));
            } else {
                state
                    .staged
                    .as_ref()
                    .unwrap()
                    .runtime
                    .materialize("controller", "secret")
                    .unwrap();
                state.applied_revision = Some(2);
                state.operate(SessionOperation::Stop {}).await.unwrap();
            }
            let before = std::fs::read(root.join("runtime.yaml")).unwrap();
            let result = state
                .operate(SessionOperation::UploadAsset {
                    path: "assets/additional".into(),
                    offset: 0,
                    bytes: b"new upload".to_vec(),
                    finished: true,
                })
                .await;
            assert!(matches!(result, Err(ServiceErrorCode::InvalidRequest)));
            assert_eq!(std::fs::read(root.join("runtime.yaml")).unwrap(), before);
            assert!(!root.join("assets/additional").exists());
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn selecting_active_validation_does_not_rewrite_either_revision_or_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-validation-selection-{}",
            random_hex().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut state = state_for_test(root.clone());
        for (revision, active) in [(1, true), (2, false)] {
            let path = root.join(format!("revision-{revision}"));
            std::fs::create_dir(&path).unwrap();
            let mut runtime =
                StagedRuntime::new(path, "tls:\n  certificate: assets/cert.pem\n").unwrap();
            runtime
                .upload("assets/cert.pem", 0, b"uploaded cert", true)
                .unwrap();
            runtime
                .materialize(
                    &format!("controller-{revision}"),
                    &format!("secret-{revision}"),
                )
                .unwrap();
            std::fs::write(
                runtime.prepared_directory().join("assets/cert.pem"),
                b"runtime asset state",
            )
            .unwrap();
            let stage = Stage {
                revision,
                runtime,
                validated: true,
                patch: None,
            };
            if active {
                state.active = Some(stage);
            } else {
                state.staged = Some(stage);
            }
        }
        let before: Vec<_> = [1, 2]
            .map(|revision| {
                std::fs::read(root.join(format!("revision-{revision}/runtime.yaml"))).unwrap()
            })
            .into();
        let validation = state.validation_config(1).unwrap();
        for (index, revision) in [1, 2].into_iter().enumerate() {
            assert_eq!(
                std::fs::read(root.join(format!("revision-{revision}/runtime.yaml"))).unwrap(),
                before[index]
            );
            assert_eq!(
                std::fs::read(root.join(format!("revision-{revision}/prepared/assets/cert.pem")))
                    .unwrap(),
                b"runtime asset state"
            );
            assert_eq!(
                std::fs::read(root.join(format!("revision-{revision}/assets/cert.pem"))).unwrap(),
                b"uploaded cert"
            );
        }
        drop(validation);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn validation_only_stage_still_accepts_upload_and_clears_validation() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-validation-upload-{}",
            random_hex().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut state = state_for_test(root.clone());
        state.staged = Some(Stage {
            revision: 2,
            runtime: StagedRuntime::new(root.clone(), "mode: rule").unwrap(),
            validated: true,
            patch: None,
        });
        state.validation_config(2).unwrap().cleanup().unwrap();
        assert!(!root.join("runtime.yaml").exists());
        state
            .operate(SessionOperation::UploadAsset {
                path: "assets/additional".into(),
                offset: 0,
                bytes: b"later upload".to_vec(),
                finished: true,
            })
            .await
            .unwrap();
        assert!(!state.staged.as_ref().unwrap().validated);
        state.validation_config(2).unwrap().cleanup().unwrap();
        assert_eq!(
            std::fs::read(root.join("prepared/assets/additional")).unwrap(),
            b"later upload"
        );
        drop(state);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn invalid_materialized_leaf_rejects_upload_without_touching_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-validation-invalid-{}",
            random_hex().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("runtime.yaml")).unwrap();
        let mut state = state_for_test(root.clone());
        state.staged = Some(Stage {
            revision: 2,
            runtime: StagedRuntime::new(root.clone(), "mode: rule").unwrap(),
            validated: true,
            patch: None,
        });
        assert!(matches!(
            state
                .operate(SessionOperation::UploadAsset {
                    path: "assets/additional".into(),
                    offset: 0,
                    bytes: b"must not upload".to_vec(),
                    finished: true,
                })
                .await,
            Err(ServiceErrorCode::InvalidAsset)
        ));
        assert!(!root.join("assets/additional").exists());
        drop(state);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn unknown_reload_protects_both_revisions_until_a_confirmed_recovery() {
        let mut state = state_for_test(
            std::env::temp_dir().join(format!("zenclash-unknown-{}", random_hex().unwrap())),
        );
        assert_eq!(
            state.observe_reload(2, Err(ServiceErrorCode::OutcomeUnknown)),
            Err(ServiceErrorCode::OutcomeUnknown)
        );
        assert_eq!(state.snapshot().unwrap().applied_revision, None);
        assert!(matches!(
            state
                .operate(SessionOperation::Stage {
                    config: "mode: rule".into()
                })
                .await,
            Err(ServiceErrorCode::InvalidRequest)
        ));
        assert!(matches!(
            state
                .operate(SessionOperation::CommitRuntime { revision: 1 })
                .await,
            Err(ServiceErrorCode::StaleRevision)
        ));
        state.observe_reload(1, Ok(())).unwrap();
        assert_eq!(state.snapshot().unwrap().applied_revision, Some(1));
        assert!(!state.runtime_unknown);
    }

    #[tokio::test]
    async fn committed_revision_survives_obsolete_resource_cleanup_failure() {
        let root = std::env::temp_dir().join(format!("zenclash-retired-{}", random_hex().unwrap()));
        // A damaged obsolete directory deliberately fails cleanup on every
        // platform, including a test runner with administrator privileges.
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("old"), b"unexpected file").unwrap();
        let mut state = state_for_test(root.clone());
        state.active = Some(Stage {
            revision: 1,
            runtime: StagedRuntime::new(root.join("old"), "mode: rule").unwrap(),
            validated: true,
            patch: None,
        });
        state.staged = Some(Stage {
            revision: 2,
            runtime: StagedRuntime::new(root.join("new"), "mode: rule").unwrap(),
            validated: true,
            patch: None,
        });
        state.applied_revision = Some(2);
        let result = state
            .operate(SessionOperation::CommitRuntime { revision: 2 })
            .await;
        let observed = state.snapshot().unwrap();
        std::fs::remove_file(root.join("old")).unwrap();
        std::fs::remove_dir(&root).unwrap();
        assert!(matches!(result, Ok(Response::Ok)));
        assert_eq!(observed.committed_revision, Some(2));
        assert!(state.retired.is_some());
    }

    #[cfg(windows)]
    #[test]
    fn runtime_cleanup_retains_shared_accepted_assets_and_failed_retired_owner() {
        use std::{fs, os::windows::fs::OpenOptionsExt};
        let fixture = Arc::new(crate::installer::OwnedTestRoot::create().unwrap());
        let session = fixture.path().join("session");
        let snapshots = session.join("snapshots/revision-1");
        let assets = session.join("assets/revision-1");
        let accepted = session.join("configurations/revision-2");
        let retired = session.join("configurations/revision-1");
        for directory in [&snapshots, &assets, &accepted, &retired] {
            fixture.create_directory(directory).unwrap();
        }
        fs::write(snapshots.join("keep"), b"approved snapshot").unwrap();
        fs::write(assets.join("keep"), b"accepted resource").unwrap();
        let retired_file = retired.join("runtime.yaml");
        fs::write(&retired_file, b"obsolete configuration").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&retired_file)
            .unwrap();
        let runtime = StagedRuntime::new_with_configuration(
            snapshots.clone(),
            accepted.clone(),
            "mode: rule",
        )
        .unwrap();
        let mut state = state_for_test(fixture.path().to_path_buf());
        state.fixture_root = Some(fixture.clone());
        state.retired = Some(runtime.retirement_for_configuration(retired.clone()));
        state.active = Some(Stage {
            revision: 2,
            runtime,
            validated: true,
            patch: None,
        });
        assert_eq!(
            state.cleanup_retired().unwrap_err().raw_os_error(),
            Some(32)
        );
        assert!(state.retired.is_some());
        assert_eq!(state.active.as_ref().unwrap().revision, 2);
        assert_eq!(fs::read(assets.join("keep")).unwrap(), b"accepted resource");
        assert_eq!(
            fs::read(snapshots.join("keep")).unwrap(),
            b"approved snapshot"
        );
        drop(held);
        state.cleanup_retired().unwrap();
        assert!(state.retired.is_none());
        assert!(!retired.exists());
        assert!(accepted.exists() && assets.exists() && snapshots.exists());
        drop(state);
        fixture.remove(&session).unwrap();
        fs::remove_dir(fixture.path()).unwrap();
    }

    #[tokio::test]
    async fn event_forwarding_stops_before_a_frame_when_the_session_is_revoked() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        let live = Arc::new(AtomicBool::new(true));
        let validity = live.clone();
        let events = stream::iter([Ok(json!({"up": 12})), Ok(json!({"up": 99}))]);
        let task = tokio::spawn(async move {
            forward_events(&mut writer, events, || {
                let validity = validity.clone();
                async move {
                    validity
                        .swap(false, Ordering::SeqCst)
                        .then_some(())
                        .ok_or(ServiceErrorCode::Expired)
                }
            })
            .await
        });
        let frame: Response = read_frame(&mut reader, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(frame, Response::Stream { data } if data == json!({"up": 12})));
        let frame: Response = read_frame(&mut reader, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(
            frame,
            Response::Error {
                code: ServiceErrorCode::Expired
            }
        ));
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_subscription_is_closed_without_waiting_for_another_kernel_event() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        let events = stream::pending::<io::Result<serde_json::Value>>();
        let task = tokio::spawn(async move {
            forward_events(&mut writer, events, || async {
                Err(ServiceErrorCode::Expired)
            })
            .await
        });
        let frame: Response = read_frame(&mut reader, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(matches!(
            frame,
            Response::Error {
                code: ServiceErrorCode::Expired
            }
        ));
        task.await.unwrap().unwrap();
    }
}
