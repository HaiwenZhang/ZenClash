// Fork integration adapted for ZenClash on 2026-10-05.
// GPL-3.0-only; see the repository and service-integration NOTICE.md.
//! Application service intents; native maintenance never owns runtime publication.

use std::{future::Future, path::PathBuf, sync::Arc};

use crate::service::{health::ServiceHealth, maintenance::MaintenanceError};
use tokio::sync::{Mutex, watch};

use crate::{CoreKind, CoreRuntimeBackend, CoreSession, CoreSessionError, TrafficCaptureSession};

mod backup;
mod health;
mod maintenance;
mod startup;
mod tun;

pub use maintenance::{
    ServiceMaintenancePreparation, ServiceMaintenanceRecoveryOutcome, ServiceMaintenanceRequest,
};

pub use backup::PreparedServiceBackupConfig;
pub use startup::verify_ordinary_local_executable;

pub use crate::service::health::ServiceHealth as ServiceHealthKind;

/// Sets the upstream application IPC budgets once during GUI bootstrap.
pub async fn configure_service_ipc() {
    zenclash_service::set_config(Some(crate::service::platform::application_ipc_config())).await;
}

/// Observes approved installation health before choosing a startup backend.
///
/// Bounded artifact checks run on a worker and native IPC verifies the peer.
/// This does not acquire ownership or start a kernel. Unreadable or uncertain
/// state is reported as Unknown and must not authorize a Local fallback.
pub async fn startup_service_health() -> ServiceHealthKind {
    match std::env::current_dir() {
        Ok(home) => health::observe(home, None).await,
        Err(_) => ServiceHealth::Unknown,
    }
}

/// Observes native service health against the application's selected core and resource home.
/// This probe starts no core and requests no administrator authorization.
pub async fn startup_service_health_for(
    home: PathBuf,
    binary: Option<PathBuf>,
) -> ServiceHealthKind {
    health::observe(home, binary).await
}

/// Application command shared by TUN controls and service maintenance controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceOperation {
    /// Refresh installation health without acquiring a service runtime lease.
    Refresh,
    /// Preserve one explicit request to install if approved and then enable TUN.
    EnableTun,
    /// Repair approved service artifacts under native authorization.
    Repair,
    /// Leave service ownership before requesting native service removal.
    Uninstall,
}

/// Prepared progress of the application's one service command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePhase {
    /// No command has been submitted.
    Idle,
    /// A background health or source check is in progress.
    Checking,
    /// The system authorization flow is in progress.
    Authorizing,
    /// The native operation remains unconfirmed and cannot be submitted again.
    Unconfirmed,
    /// The admitted core transaction is switching actual ownership.
    Switching,
    /// The admitted runtime transaction is enabling TUN.
    Enabling,
    /// The original accepted runtime is being restored.
    Restoring,
    /// The command completed; runtime activation still comes from native observations.
    Completed,
    /// The system explicitly reported cancellation of authorization.
    Cancelled,
    /// The operation failed; current core observations remain authoritative.
    Failed,
}

/// Small prepared service state; getters do not perform filesystem or IPC work.
#[derive(Clone, Debug)]
pub struct ServiceManagerSnapshot {
    operation: Option<ServiceOperation>,
    phase: ServicePhase,
    health: Option<Arc<ServiceHealth>>,
    revision: u64,
}

impl ServiceManagerSnapshot {
    /// Reads the current or most recently completed command.
    #[must_use]
    pub const fn operation(&self) -> Option<ServiceOperation> {
        self.operation
    }
    /// Reads prepared progress.
    #[must_use]
    pub const fn phase(&self) -> ServicePhase {
        self.phase
    }
    /// Reads the latest bounded background installation observation.
    #[must_use]
    pub fn health(&self) -> Option<&ServiceHealth> {
        self.health.as_deref()
    }
    /// Reads the monotonic command revision used to reject late presentation work.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Reports whether another command can currently be submitted.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            ServicePhase::Checking
                | ServicePhase::Authorizing
                | ServicePhase::Unconfirmed
                | ServicePhase::Switching
                | ServicePhase::Enabling
                | ServicePhase::Restoring
        )
    }
}

/// Failure before or during an application-owned service intent.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServiceManagerError {
    /// Another operation or unresolved native maintenance prevents a new command.
    #[error("a service operation is already in progress")]
    Busy,
    /// Core shutdown closed publication admission.
    #[error("the application is shutting down")]
    Closed,
    /// The actual core or its accepted configuration changed while authorization waited.
    #[error("the current core changed")]
    Stale,
    /// An external controller or experimental runtime cannot enter service ownership.
    #[error("the actual runtime does not support service TUN")]
    Unsupported,
    /// The explicit request did not consent to a necessary native authorization flow.
    #[error("administrator authorization requires confirmation")]
    ConsentRequired,
    /// Prepared installation facts do not permit installation or runtime acquisition.
    #[error("service installation is unavailable: {0:?}")]
    Health(ServiceHealthKind),
    /// The fixed packaged helper source could not be discovered.
    #[error("packaged service source is unavailable")]
    Sources(#[source] std::io::Error),
    /// An authenticated service lease could not be acquired or released.
    #[error("service ownership could not be established")]
    Connection(#[source] crate::service::ServiceCallError),
    /// A native authorization or installation failure.
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
    /// The admitted core transaction failed; its own recovery facts remain authoritative.
    #[error("service runtime transition failed")]
    Runtime(#[source] Box<CoreSessionError>),
    /// Completion ended unexpectedly; no new operation can assume it had no effects.
    #[error("service completion outcome is unconfirmed")]
    Completion(#[source] tokio::task::JoinError),
}

impl ServiceManagerError {
    /// Reports an explicit OS cancellation, without classifying generic I/O failures as cancellation.
    #[must_use]
    pub fn is_authorization_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Maintenance(MaintenanceError::AuthorizationCancelled)
        )
    }

    /// Reports unresolved native maintenance or a lost application command completion.
    /// Runtime recovery facts are carried separately by the capture outcome.
    #[must_use]
    pub fn is_native_outcome_unconfirmed(&self) -> bool {
        matches!(
            self,
            Self::Maintenance(MaintenanceError::OutcomeUnconfirmed(_)) | Self::Completion(_)
        )
    }
}

#[derive(Clone, Copy)]
struct ServiceIntent {
    binding: u64,
    generation: u64,
}

/// Accepted configuration outcome, including service capture and finalization facts.
pub enum ServiceConfigOutcome {
    /// Ordinary Local application of the immutable checked configuration.
    Local(crate::CoreApplyOutcome),
    /// TUN configuration accepted through authorized service ownership.
    Service(Box<crate::ServiceTunOutcome>),
}

/// One explicit TUN request paired with the runtime shown when it was created.
///
/// This is an in-memory intent, not administrator permission. A confirmation
/// may authorize a native prompt without replacing the original binding token.
#[derive(Clone)]
pub struct ServiceTunRequest {
    intent: ServiceIntent,
    backend: CoreRuntimeBackend,
    source_home: Option<PathBuf>,
    core_source: Option<PathBuf>,
    allow_authorization: bool,
    recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
    expected_capture_revision: Option<u64>,
    prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
}

impl ServiceTunRequest {
    /// Reports whether this local-to-service request still needs explicit consent.
    #[must_use]
    pub fn needs_authorization_consent(&self) -> bool {
        self.backend == CoreRuntimeBackend::Local && !self.allow_authorization
    }

    /// Allows the fixed native authorization flow for this existing intent.
    #[must_use]
    pub fn with_authorization(mut self) -> Self {
        self.allow_authorization = true;
        self
    }
}

/// Shared application command owner; it never creates a second runtime owner.
///
/// The core session remains the authority for shutdown, publication and capture
/// admission. OS authorization runs without holding its capture/transition gates.
#[derive(Clone)]
pub struct ServiceManager {
    session: CoreSession,
    capture: TrafficCaptureSession,
    command: Arc<Mutex<()>>,
    state: watch::Sender<ServiceManagerSnapshot>,
}

impl ServiceManager {
    /// Creates a service workflow for the application's existing core session.
    /// The capture session must share its publication gate; mismatched owners
    /// are rejected before authorization or runtime effects.
    #[must_use]
    pub fn new(session: CoreSession, capture: TrafficCaptureSession) -> Self {
        let (state, _) = watch::channel(ServiceManagerSnapshot {
            operation: None,
            phase: ServicePhase::Idle,
            health: None,
            revision: 0,
        });
        Self {
            session,
            capture,
            command: Arc::new(Mutex::new(())),
            state,
        }
    }

    /// Reads a small prepared snapshot without doing platform work.
    #[must_use]
    pub fn snapshot(&self) -> ServiceManagerSnapshot {
        let mut state = self.state.borrow().clone();
        let shared = self.session.run_state();
        if state.health.is_some() || shared.health != ServiceHealth::Unknown {
            state.health = Some(Arc::new(shared.health));
        }
        if self
            .session
            .run_state
            .maintenance_unconfirmed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            state.phase = ServicePhase::Unconfirmed;
        } else if shared.op_in_flight && !state.is_busy() {
            state.phase = ServicePhase::Checking;
        }
        state
    }

    /// Subscribes to prepared state changes for foreground presentation.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<ServiceManagerSnapshot> {
        self.state.subscribe()
    }

    /// Captures the actual runtime before opening a confirmation surface.
    /// Reading this request does not inspect files, perform IPC or grant privileges.
    ///
    /// # Errors
    /// Rejects shutdown, external controllers, and experimental runtimes.
    pub fn request_enable_tun(&self) -> Result<ServiceTunRequest, ServiceManagerError> {
        if !self.capture.shares_core_session(&self.session) {
            return Err(ServiceManagerError::Unsupported);
        }
        if self.session.is_shutting_down() {
            return Err(ServiceManagerError::Closed);
        }
        let descriptor = self.session.runtime_descriptor();
        if descriptor.kind() != CoreKind::Mihomo
            || !matches!(
                descriptor.backend(),
                CoreRuntimeBackend::Local | CoreRuntimeBackend::Service
            )
        {
            return Err(ServiceManagerError::Unsupported);
        }
        let request = ServiceTunRequest {
            intent: ServiceIntent {
                binding: descriptor.binding_generation(),
                generation: self.session.generation(),
            },
            backend: descriptor.backend(),
            source_home: descriptor.home_dir().map(std::path::Path::to_owned),
            core_source: descriptor.binary().map(std::path::Path::to_owned),
            allow_authorization: false,
            recovery_bundle: None,
            expected_capture_revision: None,
            prepared_config: None,
        };
        if request.backend == CoreRuntimeBackend::Local
            && (request.source_home.is_none() || request.core_source.is_none())
        {
            return Err(ServiceManagerError::Unsupported);
        }
        self.check_intent(request.intent)?;
        Ok(request)
    }

    /// Captures a TUN request using the exact resources of a successful Local recovery.
    ///
    /// Does not reread deleted sources or grant administrator authorization. The
    /// request retains one bounded immutable snapshot while confirmation is pending.
    ///
    /// # Errors
    /// Rejects failed recovery, another session, changed binding/generation or shutdown.
    pub fn request_enable_tun_after_recovery(
        &self,
        recovery: &crate::CoreLocalRecoveryOutcome,
    ) -> Result<ServiceTunRequest, ServiceManagerError> {
        let mut request = self.request_enable_tun()?;
        let bundle = recovery
            .held_bundle_for(&self.session)
            .map_err(|_| ServiceManagerError::Stale)?;
        request.recovery_bundle = Some(bundle);
        self.check_intent(request.intent)?;
        Ok(request)
    }

    /// Refreshes health only after explicit demand; no periodic artifact hashing occurs.
    ///
    /// # Errors
    /// Returns `Closed` during shutdown, `Busy` while a command is retained,
    /// or `Completion` if the independent observation task ends unexpectedly.
    pub async fn refresh_health(&self) -> Result<ServiceHealth, ServiceManagerError> {
        self.complete(ServiceOperation::Refresh, |manager| async move {
            let health = manager.observe_health().await;
            manager
                .state
                .send_modify(|state| state.health = Some(Arc::new(health.clone())));
            Ok(health)
        })
        .await
    }

    fn intent(&self) -> ServiceIntent {
        ServiceIntent {
            binding: self.session.runtime_descriptor().binding_generation(),
            generation: self.session.generation(),
        }
    }

    fn check_intent(&self, intent: ServiceIntent) -> Result<(), ServiceManagerError> {
        if !self.capture.shares_core_session(&self.session) {
            return Err(ServiceManagerError::Unsupported);
        }
        if self.session.is_shutting_down() {
            return Err(ServiceManagerError::Closed);
        }
        let current = self.intent();
        if current.binding != intent.binding || current.generation != intent.generation {
            return Err(ServiceManagerError::Stale);
        }
        Ok(())
    }

    async fn authorize_action<A>(
        &self,
        intent: ServiceIntent,
        action: crate::service::health::PendingAction,
        authorization: A,
    ) -> Result<(), ServiceManagerError>
    where
        A: Future<Output = Result<(), MaintenanceError>> + Send,
    {
        self.check_intent(intent)?;
        let sidecar_was_allowed = self.session.run_state.store.request_action(action);
        let result = self
            .authorize_then(intent, authorization, || async { Ok(()) })
            .await;
        // A cancelled or failed installer may already have changed registration.
        // Keep typed authorization errors, and retain pending state if completion is unknown.
        if !result
            .as_ref()
            .err()
            .is_some_and(ServiceManagerError::is_native_outcome_unconfirmed)
        {
            let health = self.observe_health().await;
            self.state
                .send_modify(|state| state.health = Some(Arc::new(health)));
            // Match upstream's failed-action rollback, retaining ZenClash's
            // binding receipt and unknown-outcome admission protections.
            if result.is_err() && sidecar_was_allowed && self.check_intent(intent).is_ok() {
                self.session.run_state.store.restore_sidecar_allowance();
            }
        }
        result
    }

    async fn authorize_then<T, A, C, F>(
        &self,
        intent: ServiceIntent,
        authorization: A,
        completion: C,
    ) -> Result<T, ServiceManagerError>
    where
        A: Future<Output = Result<(), MaintenanceError>> + Send,
        C: FnOnce() -> F + Send,
        F: Future<Output = Result<T, ServiceManagerError>> + Send,
    {
        self.check_intent(intent)?;
        self.state
            .send_modify(|state| state.phase = ServicePhase::Authorizing);
        authorization.await?;
        self.check_intent(intent)?;
        completion().await
    }

    async fn complete<T, C, F>(
        &self,
        operation: ServiceOperation,
        completion: C,
    ) -> Result<T, ServiceManagerError>
    where
        T: Send + 'static,
        C: FnOnce(Self) -> F + Send + 'static,
        F: Future<Output = Result<T, ServiceManagerError>> + Send + 'static,
    {
        if self.session.is_shutting_down() {
            return Err(ServiceManagerError::Closed);
        }
        if self.snapshot().phase == ServicePhase::Unconfirmed {
            return Err(ServiceManagerError::Busy);
        }
        let guard = self
            .command
            .clone()
            .try_lock_owned()
            .map_err(|_| ServiceManagerError::Busy)?;
        self.state.send_modify(|state| {
            state.operation = Some(operation);
            state.phase = ServicePhase::Checking;
            state.revision = state.revision.saturating_add(1);
        });
        let manager = self.clone();
        let result = tokio::spawn(async move {
            let _guard = guard;
            let store = manager.session.run_state.store.clone();
            let result = match store.begin_operation() {
                Err(_) => Err(ServiceManagerError::Busy),
                Ok(_operation) => {
                    if manager
                        .session
                        .run_state
                        .maintenance_unconfirmed
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        Err(ServiceManagerError::Busy)
                    } else {
                        let mut scope = ServiceCommandCompletion {
                            state: manager.session.run_state.clone(),
                            settled: false,
                        };
                        let result = completion(manager.clone()).await;
                        if result
                            .as_ref()
                            .err()
                            .is_some_and(ServiceManagerError::is_native_outcome_unconfirmed)
                        {
                            scope
                                .state
                                .maintenance_unconfirmed
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                        scope.settled = true;
                        result
                    }
                }
            };
            manager.state.send_modify(|state| {
                state.phase = match &result {
                    Ok(_) => ServicePhase::Completed,
                    Err(ServiceManagerError::Maintenance(
                        MaintenanceError::AuthorizationCancelled,
                    )) => ServicePhase::Cancelled,
                    Err(ServiceManagerError::Maintenance(
                        MaintenanceError::OutcomeUnconfirmed(_),
                    )) => ServicePhase::Unconfirmed,
                    Err(_) => ServicePhase::Failed,
                }
            });
            result
        })
        .await;
        match result {
            Ok(result) => result,
            Err(error) => {
                self.state
                    .send_modify(|state| state.phase = ServicePhase::Unconfirmed);
                Err(ServiceManagerError::Completion(error))
            }
        }
    }
}

// An unwinding retained task cannot silently reopen native authorization.
struct ServiceCommandCompletion {
    state: Arc<crate::core_session::run_state::CoreRunState>,
    settled: bool,
}

impl Drop for ServiceCommandCompletion {
    fn drop(&mut self) {
        if !self.settled {
            self.state
                .maintenance_unconfirmed
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreKind, MihomoClient, MihomoEndpoint};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    fn manager_for_session(session: CoreSession) -> ServiceManager {
        let capture = TrafficCaptureSession::new(
            session.clone(),
            crate::ControlledConfigStore::new(std::env::temp_dir().join("manager-command-fixture")),
            None,
            None,
        );
        ServiceManager::new(session, capture)
    }

    fn manager() -> ServiceManager {
        manager_for_session(
            CoreSession::open(
                CoreKind::Mihomo,
                MihomoClient::new(MihomoEndpoint::default()).unwrap(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn an_external_controller_request_cannot_install_or_change_runtime_kind() {
        let manager = manager();
        let generation = manager.session.generation();
        assert!(matches!(
            manager.request_enable_tun(),
            Err(ServiceManagerError::Unsupported)
        ));
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
        assert_eq!(manager.session.generation(), generation);
        assert_eq!(
            manager.session.runtime_descriptor().backend(),
            CoreRuntimeBackend::Direct
        );
    }

    #[tokio::test]
    async fn a_capture_owner_from_another_session_is_rejected_before_authorization() {
        let original = manager();
        let other = manager();
        let mismatched = ServiceManager::new(original.session.clone(), other.capture.clone());
        let effects = AtomicUsize::new(0);
        let authorizations = AtomicUsize::new(0);
        let result = mismatched
            .authorize_then(
                mismatched.intent(),
                async {
                    authorizations.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                || async {
                    effects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert!(matches!(result, Err(ServiceManagerError::Unsupported)));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(authorizations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn requesting_service_tun_does_not_replace_an_experimental_runtime() {
        let session = CoreSession::open(
            CoreKind::Meow,
            MihomoClient::new(MihomoEndpoint::default())
                .unwrap()
                .with_core_kind(CoreKind::Meow)
                .unwrap(),
        )
        .unwrap();
        let manager = manager_for_session(session);
        assert!(matches!(
            manager.request_enable_tun(),
            Err(ServiceManagerError::Unsupported)
        ));
        assert_eq!(manager.session.kind(), CoreKind::Meow);
        assert_eq!(manager.snapshot().phase(), ServicePhase::Idle);
    }

    #[tokio::test]
    async fn authorization_of_an_old_intent_cannot_enable_the_replacement_binding() {
        let manager = manager();
        let intent = manager.intent();
        let effects = AtomicUsize::new(0);
        manager
            .session
            .switch_to_direct(MihomoEndpoint {
                secret: "replacement-fixture".into(),
                ..MihomoEndpoint::default()
            })
            .await
            .unwrap();
        let generation = manager.session.generation();
        let result = manager
            .authorize_then(intent, async { Ok(()) }, || async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(ServiceManagerError::Stale)));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(manager.session.generation(), generation);
    }

    #[tokio::test]
    async fn cancelling_authorization_never_enters_runtime_completion() {
        let manager = manager();
        let intent = manager.intent();
        let effects = AtomicUsize::new(0);
        let result = manager
            .authorize_then(
                intent,
                async { Err(MaintenanceError::AuthorizationCancelled) },
                || async {
                    effects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(ServiceManagerError::Maintenance(
                MaintenanceError::AuthorizationCancelled
            ))
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(manager.session.generation(), intent.generation);
    }

    #[tokio::test]
    async fn shutdown_while_authorization_waits_expires_tun_intent() {
        let manager = manager();
        let intent = manager.intent();
        let effects = AtomicUsize::new(0);
        let result = manager
            .authorize_then(
                intent,
                async {
                    manager.session.request_shutdown();
                    Ok(())
                },
                || async {
                    effects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
        assert!(matches!(result, Err(ServiceManagerError::Closed)));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn abandoning_the_foreground_waiter_does_not_abandon_the_command() {
        let manager = manager();
        let effects = Arc::new(AtomicUsize::new(0));
        let effects_completion = effects.clone();
        let (entered_sender, entered) = tokio::sync::oneshot::channel();
        let (continue_sender, resume) = tokio::sync::oneshot::channel();
        let waiter_manager = manager.clone();
        let waiter = tokio::spawn(async move {
            waiter_manager
                .complete(ServiceOperation::EnableTun, |_| async move {
                    entered_sender.send(()).unwrap();
                    resume.await.unwrap();
                    effects_completion.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        entered.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        let _ = continue_sender.send(());
        tokio::time::timeout(Duration::from_secs(2), async {
            while effects.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
    }

    #[tokio::test]
    async fn a_second_command_cannot_repeat_an_authorization_in_progress() {
        let manager = manager();
        let (entered_sender, entered) = tokio::sync::oneshot::channel();
        let (continue_sender, resume) = tokio::sync::oneshot::channel();
        let first_manager = manager.clone();
        let first = tokio::spawn(async move {
            first_manager
                .complete(ServiceOperation::EnableTun, |_| async move {
                    entered_sender.send(()).unwrap();
                    resume.await.unwrap();
                    Ok(())
                })
                .await
        });
        entered.await.unwrap();
        assert!(matches!(
            manager
                .complete::<(), _, _>(ServiceOperation::EnableTun, |_| async {
                    panic!("duplicate command requested another authorization")
                })
                .await,
            Err(ServiceManagerError::Busy)
        ));
        continue_sender.send(()).unwrap();
        first.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn independent_managers_share_the_retained_operation_slot() {
        let manager = manager();
        let independent = manager_for_session(manager.session.clone());
        let (entered_sender, entered) = tokio::sync::oneshot::channel();
        let (continue_sender, resume) = tokio::sync::oneshot::channel();
        let waiter_manager = manager.clone();
        let first = tokio::spawn(async move {
            waiter_manager
                .complete(ServiceOperation::Repair, |_| async move {
                    entered_sender.send(()).unwrap();
                    resume.await.unwrap();
                    Ok(())
                })
                .await
        });
        entered.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert!(independent.snapshot().is_busy());
        assert!(matches!(
            independent
                .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
                    panic!("an independent manager repeated retained native authorization")
                })
                .await,
            Err(ServiceManagerError::Busy)
        ));
        continue_sender.send(()).unwrap();
        let mut updates = manager.session.subscribe_run_state();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.session.run_state().op_in_flight {
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(manager.snapshot().phase(), ServicePhase::Completed);
        independent
            .complete(ServiceOperation::Refresh, |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_panicking_retained_command_blocks_new_managers_in_the_same_session() {
        let manager = manager();
        let independent = manager_for_session(manager.session.clone());
        let result = manager
            .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
                panic!("simulated loss of native maintenance completion")
            })
            .await;
        assert!(matches!(result, Err(ServiceManagerError::Completion(_))));
        assert_eq!(independent.snapshot().phase(), ServicePhase::Unconfirmed);
        assert!(matches!(
            independent
                .complete::<(), _, _>(ServiceOperation::Repair, |_| async {
                    panic!("unconfirmed maintenance was resubmitted")
                })
                .await,
            Err(ServiceManagerError::Busy)
        ));
    }

    #[tokio::test]
    async fn unresolved_native_completion_cannot_be_bypassed_by_recreating_a_manager() {
        let manager = manager();
        let independent = manager_for_session(manager.session.clone());
        let error = tokio::spawn(async { panic!("native worker did not return") })
            .await
            .unwrap_err();
        let result = manager
            .complete::<(), _, _>(ServiceOperation::Uninstall, |_| async move {
                Err(ServiceManagerError::Maintenance(
                    MaintenanceError::OutcomeUnconfirmed(error),
                ))
            })
            .await;
        assert!(result.unwrap_err().is_native_outcome_unconfirmed());
        assert_eq!(independent.snapshot().phase(), ServicePhase::Unconfirmed);
        assert!(matches!(
            independent.refresh_health().await,
            Err(ServiceManagerError::Busy)
        ));
    }
    #[tokio::test]
    async fn cancelled_native_authorization_retires_pending_action_without_losing_its_error() {
        use crate::service::health::PendingAction;
        let manager = manager();
        let intent = manager.intent();
        let result = manager
            .authorize_action(intent, PendingAction::Install, async {
                assert_eq!(
                    manager.session.run_state().pending,
                    Some(PendingAction::Install)
                );
                Err(MaintenanceError::AuthorizationCancelled)
            })
            .await;
        assert!(result.unwrap_err().is_authorization_cancelled());
        assert_eq!(manager.session.run_state().pending, None);
        assert!(
            !manager
                .session
                .run_state
                .maintenance_unconfirmed
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }

    #[tokio::test]
    async fn an_unknown_native_authorization_keeps_the_requested_action_pending() {
        use crate::service::health::PendingAction;
        let manager = manager();
        let intent = manager.intent();
        let error = tokio::spawn(async { panic!("unknown native outcome") })
            .await
            .unwrap_err();
        let result = manager
            .complete::<(), _, _>(ServiceOperation::Repair, move |manager| async move {
                manager
                    .authorize_action(intent, PendingAction::ForceReinstall, async move {
                        Err(MaintenanceError::OutcomeUnconfirmed(error))
                    })
                    .await
            })
            .await;
        assert!(result.unwrap_err().is_native_outcome_unconfirmed());
        assert_eq!(
            manager.session.run_state().pending,
            Some(PendingAction::ForceReinstall)
        );
        assert_eq!(manager.snapshot().phase(), ServicePhase::Unconfirmed);
    }

    #[tokio::test]
    async fn cancelled_native_action_restores_the_same_sessions_previous_sidecar_choice() {
        use crate::service::health::PendingAction;
        let manager = manager();
        manager.session.run_state.store.accept_sidecar();
        let result = manager
            .complete(ServiceOperation::Repair, |manager| async move {
                manager
                    .authorize_action(manager.intent(), PendingAction::ForceReinstall, async {
                        assert!(!manager.session.run_state().sidecar_allowed);
                        Err(MaintenanceError::AuthorizationCancelled)
                    })
                    .await
            })
            .await;
        assert!(result.unwrap_err().is_authorization_cancelled());
        assert!(manager.session.run_state().sidecar_allowed);
        assert_eq!(manager.session.run_state().pending, None);
        assert!(!manager.session.run_state().service_needs_attention());
    }

    #[tokio::test]
    async fn unknown_native_action_cannot_restore_a_previous_sidecar_allowance() {
        use crate::service::health::PendingAction;
        let manager = manager();
        manager.session.run_state.store.accept_sidecar();
        let result = manager
            .complete(ServiceOperation::Repair, |manager| async move {
                manager
                    .authorize_action(manager.intent(), PendingAction::ForceReinstall, async {
                        let error =
                            tokio::spawn(async { panic!("native fixture lost completion") })
                                .await
                                .unwrap_err();
                        Err(MaintenanceError::OutcomeUnconfirmed(error))
                    })
                    .await
            })
            .await;
        assert!(result.unwrap_err().is_native_outcome_unconfirmed());
        assert!(!manager.session.run_state().sidecar_allowed);
        assert_eq!(
            manager.session.run_state().pending,
            Some(PendingAction::ForceReinstall)
        );
        assert_eq!(manager.snapshot().phase(), ServicePhase::Unconfirmed);
    }
}
