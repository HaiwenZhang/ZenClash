//! Serialized, intent-oriented access to runtime-core transitions.

use std::{
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::RwLock;
use thiserror::Error;
use tokio::runtime::Handle;

use crate::{
    CaptureOutcome, ControlledConfigError, ControlledConfigStore, CoreKind, MihomoClient,
    MihomoError, MihomoProcess, TrafficCaptureSession,
    controlled_config::{RuntimeApplicationTransaction, RuntimeCandidateValidation},
};

const CORE_READY_TIMEOUT: Duration = Duration::from_secs(20);
const CORE_SUPERVISOR_INTERVAL: Duration = Duration::from_millis(250);
const CORE_RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_CORE_RECOVERY_ATTEMPTS: u32 = 3;

#[derive(Clone, Copy)]
struct CoreRecoveryPolicy {
    interval: Duration,
    retry_delay: Duration,
    ready_timeout: Duration,
    max_attempts: u32,
}

type CoreRecoveryHookFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

trait CoreRecoveryCapture: Send + Sync {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_>;
    fn reconcile(&self) -> CoreRecoveryHookFuture<'_>;
}

impl CoreRecoveryCapture for TrafficCaptureSession {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move {
            match self.release_owned().await {
                Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move {
            match self.reconcile().await {
                Ok(CaptureOutcome::ReconcileNeeded { failure, .. }) => Err(failure),
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

impl Default for CoreRecoveryPolicy {
    fn default() -> Self {
        Self {
            interval: CORE_SUPERVISOR_INTERVAL,
            retry_delay: CORE_RECOVERY_RETRY_DELAY,
            ready_timeout: CORE_READY_TIMEOUT,
            max_attempts: MAX_CORE_RECOVERY_ATTEMPTS,
        }
    }
}

/// Effective-configuration change requested by a runtime caller.
#[derive(Clone, Debug)]
pub enum EffectiveConfigIntent {
    /// Merge and persist a JSON patch over the active source profile.
    Patch {
        /// Active source profile.
        profile: PathBuf,
        /// Recursive JSON object patch.
        patch: serde_json::Value,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
    /// Apply a different profile without changing its source file.
    ActivateProfile {
        /// Candidate source profile.
        profile: PathBuf,
        /// Ordered explicit YAML override files.
        overrides: Vec<PathBuf>,
    },
}

/// Maintenance transition requested for a managed runtime core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreMaintenanceIntent {
    /// Restart the owned child and wait for its controller.
    Restart,
}

/// Mechanism that successfully applied an effective configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreApplyKind {
    /// The controller accepted a complete hot reload.
    HotReloaded,
    /// A managed child restarted with the generated runtime cache.
    Restarted,
}

/// Successful configuration transition and its monotonically increasing generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreApplyOutcome {
    /// Mechanism used for the accepted change.
    pub kind: CoreApplyKind,
    /// Session generation after the transition.
    pub generation: u64,
}

/// Point-in-time state exposed by [`CoreSession`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreSessionSnapshot {
    /// Concrete runtime core.
    pub kind: CoreKind,
    /// Whether ZenClash owns the core process.
    pub managed: bool,
    /// Whether the managed process is running, or the external session remains attached.
    pub running: bool,
    /// Number of successful runtime transitions.
    pub generation: u64,
}

/// Managed-core lifecycle phase observed by the session supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLifecyclePhase {
    /// The managed child is running and no recovery is in progress.
    Stable,
    /// An unexpected exit is being recovered with a bounded retry policy.
    Recovering,
    /// Recovery attempts were exhausted and no child is running.
    Failed,
    /// An explicit shutdown has started; recovery is permanently suppressed.
    ShuttingDown,
    /// The managed child was explicitly stopped and reaped.
    Stopped,
    /// The controller belongs to an external process ZenClash cannot supervise.
    External,
}

/// Point-in-time state of managed-core recovery and shutdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreLifecycleSnapshot {
    /// Current lifecycle phase.
    pub phase: CoreLifecyclePhase,
    /// Recovery attempts made for the latest unexpected exit.
    pub recovery_attempts: u32,
    /// Exit status captured from the owned child handle.
    pub exit_reason: Option<String>,
    /// Last failed recovery operation.
    pub last_error: Option<String>,
}

impl CoreLifecycleSnapshot {
    fn new(managed: bool) -> Self {
        Self {
            phase: if managed {
                CoreLifecyclePhase::Stable
            } else {
                CoreLifecyclePhase::External
            },
            recovery_attempts: 0,
            exit_reason: None,
            last_error: None,
        }
    }
}

/// Errors returned by intent-oriented core transitions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreSessionError {
    /// Effective configuration preparation, application, or persistence failed.
    #[error(transparent)]
    Config(#[from] ControlledConfigError),
    /// Managed process transition failed.
    #[error(transparent)]
    Process(#[from] MihomoError),
    /// An external core would require a process restart ZenClash does not own.
    #[error("外部 {core} 内核不支持需要重启的运行时变更")]
    ExternalRestartUnsupported {
        /// External runtime-core implementation.
        core: CoreKind,
    },
    /// An operation raced with explicit application shutdown.
    #[error("内核会话正在停止，拒绝开始新的运行时操作")]
    ShuttingDown,
}

/// Cloneable owner of serialized effective-config and lifecycle transitions.
#[derive(Clone)]
pub struct CoreSession {
    kind: CoreKind,
    client: MihomoClient,
    process: Option<Arc<MihomoProcess>>,
    transition: Arc<tokio::sync::Mutex<()>>,
    generation: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
    supervisor_started: Arc<AtomicBool>,
    lifecycle: Arc<RwLock<CoreLifecycleSnapshot>>,
}

#[must_use]
pub(crate) struct CoreProfileApplication {
    state: CoreProfileApplicationState,
    generation: Arc<AtomicU64>,
    _transition_guard: tokio::sync::OwnedMutexGuard<()>,
}

enum CoreProfileApplicationState {
    Runtime {
        transaction: Box<RuntimeApplicationTransaction>,
        kind: CoreApplyKind,
    },
    Validated(RuntimeCandidateValidation),
}

impl CoreProfileApplication {
    pub(crate) fn commit(self) -> Option<CoreApplyOutcome> {
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, kind } => {
                transaction.commit();
                let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                Some(CoreApplyOutcome { kind, generation })
            }
            CoreProfileApplicationState::Validated(_validation) => None,
        }
    }

    pub(crate) async fn rollback(self) -> Result<(), CoreSessionError> {
        match self.state {
            CoreProfileApplicationState::Runtime { transaction, .. } => transaction
                .rollback()
                .await
                .map_err(CoreSessionError::Config),
            CoreProfileApplicationState::Validated(_) => Ok(()),
        }
    }
}

impl CoreSession {
    /// Opens a session over one controller and its optional owned child process.
    #[must_use]
    pub fn open(kind: CoreKind, client: MihomoClient, process: Option<Arc<MihomoProcess>>) -> Self {
        debug_assert!(
            process
                .as_ref()
                .is_none_or(|process| process.kind() == kind)
        );
        Self {
            kind,
            client,
            lifecycle: Arc::new(RwLock::new(CoreLifecycleSnapshot::new(process.is_some()))),
            process,
            transition: Arc::new(tokio::sync::Mutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            supervisor_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Starts one bounded unexpected-exit supervisor for a managed core.
    ///
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor(&self, runtime: &Handle) -> bool {
        self.start_supervisor_with_policy(runtime, CoreRecoveryPolicy::default(), None)
    }

    /// Starts managed-core supervision with capture release and restoration hooks.
    ///
    /// On an unexpected exit, owned native capture is released before restart;
    /// after controller readiness succeeds, persistent intent is reconciled.
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor_with_capture(
        &self,
        runtime: &Handle,
        capture: TrafficCaptureSession,
    ) -> bool {
        self.start_supervisor_with_policy(
            runtime,
            CoreRecoveryPolicy::default(),
            Some(Arc::new(capture)),
        )
    }

    fn start_supervisor_with_policy(
        &self,
        runtime: &Handle,
        policy: CoreRecoveryPolicy,
        capture: Option<Arc<dyn CoreRecoveryCapture>>,
    ) -> bool {
        if self.process.is_none() || self.supervisor_started.swap(true, Ordering::AcqRel) {
            return false;
        }
        let session = self.clone();
        runtime.spawn(async move { supervise_managed_core(session, policy, capture).await });
        true
    }

    /// Returns the concrete runtime core.
    #[must_use]
    pub const fn kind(&self) -> CoreKind {
        self.kind
    }

    /// Returns the shared query client associated with the session.
    #[must_use]
    pub const fn client(&self) -> &MihomoClient {
        &self.client
    }

    /// Applies an effective-configuration intent through hot reload or managed restart.
    ///
    /// Explicit API rejection is returned directly. A managed core falls back
    /// to restart only after transport or response-decoding failure, where the
    /// hot-reload outcome is uncertain. External cores are never restarted.
    ///
    /// # Errors
    ///
    /// Returns preparation, validation, controller, persistence, or process errors.
    pub async fn apply(
        &self,
        store: &ControlledConfigStore,
        intent: EffectiveConfigIntent,
    ) -> Result<CoreApplyOutcome, CoreSessionError> {
        let _transition = self.transition.lock().await;
        self.ensure_running_operations_allowed()?;
        let kind = match intent {
            EffectiveConfigIntent::Patch {
                profile,
                patch,
                overrides,
            } => self.apply_patch(store, profile, patch, overrides).await?,
            EffectiveConfigIntent::ActivateProfile { profile, overrides } => {
                self.activate_profile(store, profile, overrides).await?
            }
        };
        Ok(CoreApplyOutcome {
            kind,
            generation: self.next_generation(),
        })
    }

    pub(crate) async fn stage_profile_application(
        &self,
        store: &ControlledConfigStore,
        candidate: PathBuf,
        previous: Option<PathBuf>,
        overrides: Vec<PathBuf>,
        apply_runtime: bool,
    ) -> Result<CoreProfileApplication, CoreSessionError> {
        let transition_guard = self.transition.clone().lock_owned().await;
        self.ensure_running_operations_allowed()?;
        let state = if !apply_runtime {
            CoreProfileApplicationState::Validated(
                store
                    .stage_profile_validation(self.kind, &self.client, candidate, overrides)
                    .await?,
            )
        } else if !self.kind.capabilities().full_config_reload {
            CoreProfileApplicationState::Runtime {
                transaction: Box::new(
                    store
                        .stage_profile_restart(
                            self.require_managed_restart()?,
                            candidate,
                            overrides,
                        )
                        .await?,
                ),
                kind: CoreApplyKind::Restarted,
            }
        } else {
            match store
                .stage_profile_reload(&self.client, candidate.clone(), previous, overrides.clone())
                .await
            {
                Ok(transaction) => CoreProfileApplicationState::Runtime {
                    transaction: Box::new(transaction),
                    kind: CoreApplyKind::HotReloaded,
                },
                Err(error) if should_restart_after_hot_reload(&error) && self.process.is_some() => {
                    CoreProfileApplicationState::Runtime {
                        transaction: Box::new(
                            store
                                .stage_profile_restart(
                                    self.process.clone().expect("managed process checked"),
                                    candidate,
                                    overrides,
                                )
                                .await?,
                        ),
                        kind: CoreApplyKind::Restarted,
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };
        Ok(CoreProfileApplication {
            state,
            generation: self.generation.clone(),
            _transition_guard: transition_guard,
        })
    }

    /// Performs a serialized managed-core maintenance transition.
    ///
    /// # Errors
    ///
    /// Returns an error for external cores or when restart/readiness fails.
    pub async fn maintain(&self, intent: CoreMaintenanceIntent) -> Result<u64, CoreSessionError> {
        self.maintain_with_timeout(intent, CORE_READY_TIMEOUT).await
    }

    async fn maintain_with_timeout(
        &self,
        intent: CoreMaintenanceIntent,
        timeout: Duration,
    ) -> Result<u64, CoreSessionError> {
        let _transition = self.transition.lock().await;
        self.ensure_running_operations_allowed()?;
        let process = self
            .process
            .as_ref()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })?;
        match intent {
            CoreMaintenanceIntent::Restart => {
                process
                    .restart_and_wait_until(timeout, Some(self.shutdown_requested.clone()))
                    .await?;
            }
        }
        *self.lifecycle.write() = CoreLifecycleSnapshot::new(true);
        Ok(self.next_generation())
    }

    /// Stops the managed child, or detaches from an external controller.
    ///
    /// # Errors
    ///
    /// Returns an error when an owned child cannot be stopped and reaped.
    pub async fn shutdown(&self) -> Result<(), CoreSessionError> {
        self.shutdown_requested.store(true, Ordering::Release);
        if self.process.is_some() {
            self.lifecycle.write().phase = CoreLifecyclePhase::ShuttingDown;
        }
        let _transition = self.transition.lock().await;
        if let Some(process) = &self.process {
            process.stop_async().await?;
            self.next_generation();
            self.lifecycle.write().phase = CoreLifecyclePhase::Stopped;
        }
        Ok(())
    }

    /// Returns managed recovery attempts, exit reason, and terminal phase.
    #[must_use]
    pub fn lifecycle_snapshot(&self) -> CoreLifecycleSnapshot {
        self.lifecycle.read().clone()
    }

    /// Captures ownership, running state, and successful transition generation.
    #[must_use]
    pub fn snapshot(&self) -> CoreSessionSnapshot {
        CoreSessionSnapshot {
            kind: self.kind,
            managed: self.process.is_some(),
            running: self
                .process
                .as_ref()
                .is_none_or(|process| process.is_running()),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    pub(crate) fn managed_process_snapshot(&self) -> Option<crate::MihomoProcessSnapshot> {
        self.process.as_ref().map(|process| process.snapshot())
    }

    async fn apply_patch(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        patch: serde_json::Value,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            let process = self.require_managed_restart()?;
            store
                .apply_json_update_with_restart(process, profile, &patch, overrides)
                .await?;
            return Ok(CoreApplyKind::Restarted);
        }

        match store
            .apply_json_update_with_overrides(&self.client, &profile, &patch, overrides.clone())
            .await
        {
            Ok(()) => Ok(CoreApplyKind::HotReloaded),
            Err(error) if should_restart_after_hot_reload(&error) && self.process.is_some() => {
                store
                    .apply_json_update_with_restart(
                        self.process.clone().expect("managed process checked"),
                        profile,
                        &patch,
                        overrides,
                    )
                    .await?;
                Ok(CoreApplyKind::Restarted)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn activate_profile(
        &self,
        store: &ControlledConfigStore,
        profile: PathBuf,
        overrides: Vec<PathBuf>,
    ) -> Result<CoreApplyKind, CoreSessionError> {
        if !self.kind.capabilities().full_config_reload {
            store
                .restart_with_overrides(self.require_managed_restart()?, profile, overrides)
                .await?;
            return Ok(CoreApplyKind::Restarted);
        }

        match store
            .reload_with_overrides(&self.client, &profile, overrides.clone())
            .await
        {
            Ok(()) => Ok(CoreApplyKind::HotReloaded),
            Err(error) if should_restart_after_hot_reload(&error) && self.process.is_some() => {
                store
                    .restart_with_overrides(
                        self.process.clone().expect("managed process checked"),
                        profile,
                        overrides,
                    )
                    .await?;
                Ok(CoreApplyKind::Restarted)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn require_managed_restart(&self) -> Result<Arc<MihomoProcess>, CoreSessionError> {
        self.process
            .clone()
            .ok_or(CoreSessionError::ExternalRestartUnsupported { core: self.kind })
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn ensure_running_operations_allowed(&self) -> Result<(), CoreSessionError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            Err(CoreSessionError::ShuttingDown)
        } else {
            Ok(())
        }
    }
}

async fn supervise_managed_core(
    session: CoreSession,
    policy: CoreRecoveryPolicy,
    capture: Option<Arc<dyn CoreRecoveryCapture>>,
) {
    loop {
        if session.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        let Some(process) = session.process.as_ref() else {
            return;
        };
        if process.is_running() {
            tokio::time::sleep(policy.interval).await;
            continue;
        }

        let process_snapshot = process.snapshot();
        let (new_exit, retry_exhausted) = {
            let mut lifecycle = session.lifecycle.write();
            let new_exit = lifecycle.phase == CoreLifecyclePhase::Stable;
            if new_exit {
                lifecycle.recovery_attempts = 0;
                lifecycle.exit_reason = process_snapshot
                    .exit_reason
                    .or_else(|| Some(format!("{} 托管进程意外退出", session.kind)));
                lifecycle.last_error = None;
            }
            if lifecycle.recovery_attempts >= policy.max_attempts {
                lifecycle.phase = CoreLifecyclePhase::Failed;
                (new_exit, true)
            } else {
                lifecycle.phase = CoreLifecyclePhase::Recovering;
                (new_exit, false)
            }
        };
        if retry_exhausted {
            tokio::time::sleep(policy.interval).await;
            continue;
        }
        if new_exit
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.release_owned().await
        {
            tracing::warn!(%error, "failed to release owned capture after managed-core exit");
        }

        if session.lifecycle.read().recovery_attempts > 0 {
            tokio::time::sleep(policy.retry_delay).await;
            if session.shutdown_requested.load(Ordering::Acquire) {
                return;
            }
        }

        let recovered = recover_managed_core(&session, policy).await;
        if recovered
            && let Some(capture) = capture.as_ref()
            && let Err(error) = capture.reconcile().await
        {
            tracing::warn!(%error, "failed to reconcile capture after managed-core recovery");
        }
        if recovered || session.shutdown_requested.load(Ordering::Acquire) {
            tokio::time::sleep(policy.interval).await;
        }
    }
}

async fn recover_managed_core(session: &CoreSession, policy: CoreRecoveryPolicy) -> bool {
    let _transition = session.transition.lock().await;
    if session.shutdown_requested.load(Ordering::Acquire) {
        return false;
    }
    let Some(process) = session.process.as_ref() else {
        return false;
    };
    if process.is_running() {
        session.lifecycle.write().phase = CoreLifecyclePhase::Stable;
        return true;
    }
    let attempt = {
        let mut lifecycle = session.lifecycle.write();
        lifecycle.recovery_attempts = lifecycle.recovery_attempts.saturating_add(1);
        lifecycle.phase = CoreLifecyclePhase::Recovering;
        lifecycle.recovery_attempts
    };
    match process
        .restart_and_wait_until(
            policy.ready_timeout,
            Some(session.shutdown_requested.clone()),
        )
        .await
    {
        Ok(()) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.phase = CoreLifecyclePhase::Stable;
            lifecycle.last_error = None;
            session.next_generation();
            true
        }
        Err(error) => {
            let mut lifecycle = session.lifecycle.write();
            lifecycle.last_error = Some(error.to_string());
            lifecycle.phase = if attempt >= policy.max_attempts {
                CoreLifecyclePhase::Failed
            } else {
                CoreLifecyclePhase::Recovering
            };
            false
        }
    }
}

fn should_restart_after_hot_reload(error: &ControlledConfigError) -> bool {
    matches!(error, ControlledConfigError::Profile(MihomoError::Http(_)))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::atomic::AtomicUsize;

    #[cfg(unix)]
    use crate::{MihomoEndpoint, MihomoLaunchConfig};

    use super::*;

    #[tokio::test]
    async fn accepted_profile_apply_reports_hot_reload_and_advances_generation() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
            request
        });
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-apply-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client =
            MihomoClient::new(crate::MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client, None);

        let outcome = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile,
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(outcome.kind, CoreApplyKind::HotReloaded);
        assert_eq!(outcome.generation, 1);
        assert_eq!(session.snapshot().generation, 1);
        assert!(request.starts_with("PUT /configs?force=true "));
        assert!(store.runtime_path().is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_meow_never_fakes_a_restart_capability() {
        let session = CoreSession::open(
            CoreKind::Meow,
            MihomoClient::new(crate::MihomoEndpoint::default()).unwrap(),
            None,
        );
        let store = ControlledConfigStore::new(std::env::temp_dir().join(format!(
            "zenclash-core-session-external-{}",
            std::process::id()
        )));

        let error = session
            .apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile: PathBuf::from("unused.yaml"),
                    overrides: Vec::new(),
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CoreSessionError::ExternalRestartUnsupported {
                core: CoreKind::Meow
            }
        ));
        assert_eq!(session.snapshot().generation, 0);
    }

    #[tokio::test]
    async fn uncertain_external_mihomo_apply_never_becomes_an_owned_restart() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-external-uncertain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        std::fs::write(&profile, "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client, None);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            session.apply(
                &store,
                EffectiveConfigIntent::ActivateProfile {
                    profile,
                    overrides: Vec::new(),
                },
            ),
        )
        .await
        .expect("external controller failure did not remain bounded")
        .unwrap_err();

        assert!(matches!(
            error,
            CoreSessionError::Config(ControlledConfigError::Profile(MihomoError::Http(_)))
        ));
        let snapshot = session.snapshot();
        assert!(!snapshot.managed);
        assert!(
            snapshot.running,
            "external process ownership is not inferred"
        );
        assert_eq!(snapshot.generation, 0);
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::External
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_config_rejection_is_not_a_restart_signal() {
        let error = ControlledConfigError::Profile(MihomoError::Api {
            status: 400,
            message: "invalid config".into(),
        });

        assert!(!should_restart_after_hot_reload(&error));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unexpected_exit_retries_are_bounded_and_end_in_visible_failure() {
        let root = lifecycle_test_root("bounded-recovery");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; printf crash >&2; exit 23",
                launches.display()
            ),
        );
        let process = spawn_lifecycle_process(&root, binary);
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(process.endpoint().clone()).unwrap(),
            Some(process.clone()),
        );
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_millis(50),
                max_attempts: 2,
            },
            None,
        ));
        assert!(!session.start_supervisor(&Handle::current()));

        tokio::time::timeout(Duration::from_secs(10), async {
            while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervisor did not reach bounded failure");
        let lifecycle = session.lifecycle_snapshot();
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();

        assert_eq!(lifecycle.recovery_attempts, 2);
        assert!(lifecycle.exit_reason.is_some());
        assert!(lifecycle.last_error.is_some());
        assert_eq!(launch_count, 3, "initial launch plus exactly two retries");
        assert!(!process.is_running());
        session.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manual_recovery_after_exhaustion_rearms_the_supervisor() {
        let root = lifecycle_test_root("rearmed-supervisor");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let endpoint = MihomoEndpoint::new(format!("http://{address}"), "");
        write_lifecycle_script(
            &binary,
            &format!("printf x >> '{}'; exit 23", launches.display()),
        );
        let process =
            spawn_lifecycle_process_with_endpoint(&root, binary.clone(), endpoint.clone());
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(endpoint).unwrap(),
            Some(process.clone()),
        );
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_millis(50),
                max_attempts: 1,
            },
            None,
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initial recovery did not exhaust its retry");

        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display()
            ),
        );
        let listener = TcpListener::bind(address).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"meta":true,"version":"test"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        assert_eq!(
            session
                .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(1))
                .await
                .unwrap(),
            1
        );
        server.join().unwrap();
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::Stable
        );

        process.stop_async().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let lifecycle = session.lifecycle_snapshot();
                if lifecycle.phase == CoreLifecyclePhase::Failed && lifecycle.recovery_attempts == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervisor was not rearmed after the manual recovery");
        assert!(
            (3..=4).contains(&std::fs::read_to_string(&launches).unwrap().len()),
            "the rearmed retry may be stopped before its script is scheduled"
        );
        session.shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_cancels_a_restart_waiter_and_prevents_a_late_child() {
        let root = lifecycle_test_root("shutdown-cancel");
        let launches = root.join("launches");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display()
            ),
        );
        let process = spawn_lifecycle_process(&root, binary);
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(process.endpoint().clone()).unwrap(),
            Some(process.clone()),
        );
        let restarting = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(2))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_to_string(&launches).map_or(0, |value| value.len()) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("restart did not launch its candidate child");

        tokio::time::timeout(Duration::from_secs(1), session.shutdown())
            .await
            .expect("shutdown waited for the full readiness timeout")
            .unwrap();
        let restart_error = restarting.await.unwrap().unwrap_err().to_string();
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(restart_error.contains("停止请求"));
        assert_eq!(launch_count, 2, "shutdown must not permit a later restart");
        assert!(!process.is_running());
        assert_eq!(
            session.lifecycle_snapshot().phase,
            CoreLifecyclePhase::Stopped
        );
        assert!(matches!(
            session.maintain(CoreMaintenanceIntent::Restart).await,
            Err(CoreSessionError::ShuttingDown)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_crash_recovery_releases_then_reconciles_capture_once() {
        let root = lifecycle_test_root("capture-hooks");
        let launches = root.join("launches");
        let crashed = root.join("crashed");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; if [ ! -f '{}' ]; then touch '{}'; exit 23; fi; trap 'exit 0' TERM; while true; do sleep 0.05; done",
                launches.display(),
                crashed.display(),
                crashed.display(),
            ),
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"meta":true,"version":"test"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let process = spawn_lifecycle_process_with_endpoint(&root, binary, endpoint.clone());
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(endpoint).unwrap(),
            Some(process.clone()),
        );
        let capture = Arc::new(RecordingCapture::default());
        assert!(session.start_supervisor_with_policy(
            &Handle::current(),
            CoreRecoveryPolicy {
                interval: Duration::from_millis(10),
                retry_delay: Duration::from_millis(10),
                ready_timeout: Duration::from_secs(1),
                max_attempts: 2,
            },
            Some(capture.clone()),
        ));

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let lifecycle = session.lifecycle_snapshot();
                if lifecycle.phase == CoreLifecyclePhase::Stable && lifecycle.recovery_attempts == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("managed core did not recover");

        assert_eq!(capture.releases.load(Ordering::Acquire), 1);
        assert_eq!(capture.reconciles.load(Ordering::Acquire), 1);
        assert_eq!(std::fs::read_to_string(&launches).unwrap().len(), 2);
        assert!(process.is_running());
        session.shutdown().await.unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_restarts_serialize_without_overlapping_children() {
        let root = lifecycle_test_root("serialized-restarts");
        let launches = root.join("launches");
        let guard = root.join("running");
        let overlaps = root.join("overlaps");
        let binary = root.join("mihomo");
        write_lifecycle_script(
            &binary,
            &format!(
                "printf x >> '{}'; if ! mkdir '{}' 2>/dev/null; then printf x >> '{}'; exit 99; fi; cleanup() {{ rmdir '{}' 2>/dev/null || true; }}; trap 'cleanup; exit 0' TERM INT; trap cleanup EXIT; while true; do sleep 0.05; done",
                launches.display(),
                guard.display(),
                overlaps.display(),
                guard.display(),
            ),
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let _ = stream.read(&mut request).unwrap();
                let body = r#"{"meta":true,"version":"test"}"#;
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let process = spawn_lifecycle_process_with_endpoint(&root, binary, endpoint.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !guard.is_dir() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initial managed child did not acquire its process guard");
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(endpoint).unwrap(),
            Some(process.clone()),
        );

        let first = {
            let session = session.clone();
            tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
        };
        let second = {
            let session = session.clone();
            tokio::spawn(async move { session.maintain(CoreMaintenanceIntent::Restart).await })
        };
        let (first, second) = tokio::join!(first, second);

        let generations = [first.unwrap().unwrap(), second.unwrap().unwrap()];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !guard.is_dir() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("latest managed child did not acquire its process guard");
        let launch_count = std::fs::read_to_string(&launches).unwrap().len();
        assert!(generations.contains(&1));
        assert!(generations.contains(&2));
        assert_eq!(session.snapshot().generation, 2);
        assert!(
            (2..=3).contains(&launch_count),
            "the initial and latest child must run; an immediately replaced child may not be scheduled"
        );
        assert!(!overlaps.exists(), "two managed children overlapped");
        assert!(process.is_running());
        session.shutdown().await.unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    fn lifecycle_test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("profile.yaml"), "rules:\n  - MATCH,DIRECT\n").unwrap();
        root
    }

    #[cfg(unix)]
    fn write_lifecycle_script(path: &std::path::Path, run: &str) {
        std::fs::write(
            path,
            format!("#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\n{run}\n"),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn spawn_lifecycle_process(root: &std::path::Path, binary: PathBuf) -> Arc<MihomoProcess> {
        spawn_lifecycle_process_with_endpoint(root, binary, MihomoEndpoint::default())
    }

    #[cfg(unix)]
    fn spawn_lifecycle_process_with_endpoint(
        root: &std::path::Path,
        binary: PathBuf,
        endpoint: MihomoEndpoint,
    ) -> Arc<MihomoProcess> {
        MihomoProcess::spawn(MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: root.join("profile.yaml"),
            home_dir: root.join("data"),
            endpoint,
            controller_override: None,
        })
        .unwrap()
    }

    #[cfg(unix)]
    #[derive(Default)]
    struct RecordingCapture {
        releases: AtomicUsize,
        reconciles: AtomicUsize,
    }

    #[cfg(unix)]
    impl CoreRecoveryCapture for RecordingCapture {
        fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
            self.releases.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }

        fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
            self.reconciles.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }
    }
}
