// Upstream RunState integration adapted for ZenClash on 2026-10-05.
// GPL-3.0-only; see the repository and service-integration NOTICE.md.
//! Per-session bridge to the copied RunStateStore; CoreSession owns lifecycle.

use std::sync::atomic::AtomicBool;

use crate::service::runstate::{
    NativeEnv, PendingAction, RunState, RunStateEnv, RunStateHost, RunStateStore, ServiceHealth,
    ServiceVersionReply,
};
use anyhow::Context as _;
use parking_lot::Mutex;
use tokio::sync::watch;

use super::*;
use crate::service::RunningMode;
use crate::{CoreRuntimeBackend, PacServer};

pub(crate) struct CoreRunState {
    pub(crate) store: Arc<RunStateStore<SessionEnv>>,
    effects: SessionEffects,
    source: Arc<RwLock<Option<Arc<OfflineCoreSource>>>>,
    binding: Mutex<u64>,
    mode_update: Mutex<()>,
    pub(crate) maintenance_unconfirmed: AtomicBool,
}

pub(crate) struct SessionEnv {
    client: MihomoClient,
    source: Arc<RwLock<Option<Arc<OfflineCoreSource>>>>,
    effects: SessionEffects,
    elevated: bool,
}

#[derive(Clone)]
struct SessionEffects {
    state: watch::Sender<RunState>,
    pac: Arc<Mutex<PacEffects>>,
}

#[derive(Default)]
struct PacEffects {
    server: Option<PacServer>,
    available: bool,
    attempts: usize,
    external_allowed: bool,
}

impl PacEffects {
    fn apply(&self) {
        if let Some(server) = &self.server {
            server.set_available(self.attempts == 0 && (self.available || self.external_allowed));
        }
    }
}

impl CoreRunState {
    pub(crate) fn new(client: &MihomoClient) -> Self {
        let elevated = crate::service::current_process_elevated();
        let (state, _) = watch::channel(RunState {
            health: ServiceHealth::Unknown,
            pending: None,
            sidecar_allowed: false,
            mode: RunningMode::NotRunning,
            is_admin: elevated,
            op_in_flight: false,
        });
        let effects = SessionEffects {
            state,
            pac: Arc::new(Mutex::new(PacEffects::default())),
        };
        let source = Arc::new(RwLock::new(None));
        Self {
            store: Arc::new(RunStateStore::new(SessionEnv {
                client: client.clone(),
                source: source.clone(),
                effects: effects.clone(),
                elevated,
            })),
            effects,
            source,
            binding: Mutex::new(client.runtime_descriptor().binding_generation()),
            mode_update: Mutex::new(()),
            maintenance_unconfirmed: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_offline_source(&self, source: Arc<OfflineCoreSource>) {
        *self.source.write() = Some(source);
    }

    pub(crate) fn invalidate_changed_binding(&self, binding: u64) {
        let mut previous = self.binding.lock();
        if *previous != binding {
            *previous = binding;
            // Invalidates copied-store observation reservations before publication.
            self.store.observe(ServiceHealth::Unknown);
        }
    }
}

impl CoreSession {
    /// Reads the copied upstream state from this session without I/O or waiting.
    #[must_use]
    pub fn run_state(&self) -> RunState {
        self.run_state.store.state()
    }

    /// Subscribes to background state publication; foreground users should reread `run_state`.
    #[must_use]
    pub fn subscribe_run_state(&self) -> watch::Receiver<RunState> {
        self.run_state.effects.state.subscribe()
    }

    /// Binds the actual application PAC owner without changing OS proxy settings.
    /// Managed sessions stay closed until actual liveness has been observed.
    pub fn attach_pac_server(&self, server: PacServer) {
        let mut effects = self.run_state.effects.pac.lock();
        effects.external_allowed =
            !self.is_managed() && !self.is_offline_recovery() && !self.is_shutting_down();
        effects.server = Some(server);
        effects.apply();
    }

    /// Transfers the bootstrap's native health observation into the application session.
    /// Call once on the bootstrap worker before exposing the session to GUI commands.
    /// An accepted Local fallback records the copied upstream Sidecar allowance;
    /// external controllers cannot claim service capability through this method.
    pub fn record_startup_service_health(&self, health: ServiceHealth) {
        if self.kind != CoreKind::Mihomo
            || (!self.is_managed() && !self.is_offline_recovery())
            || self.is_shutting_down()
        {
            return;
        }
        self.run_state.store.observe(health.clone());
        if self.runtime_descriptor().backend() != CoreRuntimeBackend::Local {
            return;
        }
        match health {
            ServiceHealth::NotInstalled => self.run_state.store.accept_sidecar(),
            ServiceHealth::VersionMismatch | ServiceHealth::Unavailable(_) => {
                // Native idle/ownership checks and the platform/user choice have
                // already admitted Local startup; this records its session intent.
                if let Err(error) = self.run_state.store.allow_sidecar_for_session() {
                    tracing::warn!(%error, "startup Sidecar intent could not be recorded");
                }
            }
            ServiceHealth::Ready | ServiceHealth::Unknown => {}
        }
    }

    pub(crate) async fn observe_service_health(&self) -> ServiceHealth {
        self.run_state
            .invalidate_changed_binding(self.runtime_descriptor().binding_generation());
        if self.kind != CoreKind::Mihomo || (!self.is_managed() && !self.is_offline_recovery()) {
            self.run_state.store.observe(ServiceHealth::Unknown);
            return ServiceHealth::Unknown;
        }
        self.run_state.store.observe_current_health().await
    }

    // Called only on the retained runtime worker/supervisor, never in GUI render.
    pub(crate) fn reconcile_run_state(&self) {
        let _update = self.run_state.mode_update.lock();
        let descriptor = self.runtime_descriptor();
        self.run_state
            .invalidate_changed_binding(descriptor.binding_generation());
        let lifecycle = self.lifecycle_snapshot();
        let active = !self.is_shutting_down()
            && !self.network_suspended.load(Ordering::Acquire)
            && lifecycle.phase == CoreLifecyclePhase::Stable
            && !lifecycle.stop_requested;
        let mode = if active {
            match self.client.owned_core() {
                Some(crate::owned_core::OwnedCore::Local(process))
                    if process
                        .try_snapshot()
                        .is_ok_and(|snapshot| snapshot.running) =>
                {
                    RunningMode::Sidecar
                }
                Some(crate::owned_core::OwnedCore::Service(runtime))
                    if runtime.observation_allowed()
                        && runtime.client.snapshot().is_some_and(|status| {
                            status.is_active
                                && status.core_pid.is_some()
                                && status.service_state
                                    == zenclash_service::ServiceLifecycleState::Running
                        }) =>
                {
                    RunningMode::Service
                }
                _ => RunningMode::NotRunning,
            }
        } else {
            RunningMode::NotRunning
        };
        if self.runtime_descriptor().binding_generation() != descriptor.binding_generation() {
            return;
        }
        {
            let mut effects = self.run_state.effects.pac.lock();
            effects.external_allowed = descriptor.backend() == CoreRuntimeBackend::Direct
                && !self.is_offline_recovery()
                && !self.is_shutting_down();
            effects.apply();
        }
        if self.run_state.store.state().mode != mode {
            if mode == RunningMode::NotRunning {
                self.run_state.store.core_stopped();
            } else {
                self.run_state.store.core_started(mode);
            }
        } else {
            self.run_state.store.core_start_settled();
        }
    }

    pub(crate) fn begin_core_run_attempt(&self) -> CoreRunAttempt {
        {
            let mut effects = self.run_state.effects.pac.lock();
            effects.attempts += 1;
            effects.apply();
        }
        self.run_state.store.core_starting();
        CoreRunAttempt {
            session: self.clone(),
        }
    }

    pub(crate) fn close_run_state_for_shutdown(&self) {
        let mut effects = self.run_state.effects.pac.lock();
        effects.external_allowed = false;
        effects.available = false;
        effects.apply();
        drop(effects);
        self.run_state.store.core_stopped();
    }
}

pub(crate) struct CoreRunAttempt {
    session: CoreSession,
}

impl Drop for CoreRunAttempt {
    fn drop(&mut self) {
        self.session.reconcile_run_state();
        self.session.run_state.store.core_start_settled();
        let mut effects = self.session.run_state.effects.pac.lock();
        effects.attempts -= 1;
        effects.apply();
    }
}

impl RunStateHost for SessionEffects {
    fn set_pac_available(&self, available: bool) {
        let mut pac = self.pac.lock();
        pac.available = available;
        pac.apply();
    }

    fn publish(&self, state: &RunState) {
        self.state.send_replace(state.clone());
    }

    fn run_privileged(&self, _action: PendingAction) -> anyhow::Result<()> {
        // Maintenance keeps existing typed errors and bound user-consent receipts.
        anyhow::bail!("native maintenance requires an admitted ServiceManager request")
    }
}

impl SessionEnv {
    fn source(&self) -> anyhow::Result<(PathBuf, Option<PathBuf>, u64)> {
        let descriptor = self.client.runtime_descriptor();
        let binding = descriptor.binding_generation();
        if let Some(source) = self.source.read().as_ref()
            && descriptor.backend() == CoreRuntimeBackend::Direct
            && source.binding == binding
        {
            return Ok((source.home.clone(), source.binary.clone(), binding));
        }
        let runtime = self.client.runtime_session();
        let home = descriptor
            .home_dir()
            .map(std::path::Path::to_owned)
            .or_else(|| {
                runtime
                    .as_ref()
                    .map(|runtime| runtime.source_home().to_owned())
            })
            .context("external controllers have no managed service source")?;
        let binary = descriptor
            .binary()
            .map(std::path::Path::to_owned)
            .or_else(|| {
                self.client
                    .local_recovery_launch()
                    .map(|launch| launch.binary)
            })
            .or_else(|| {
                runtime.and_then(|runtime| runtime.core_source().map(std::path::Path::to_owned))
            });
        Ok((home, binary, binding))
    }
}

impl RunStateEnv for SessionEnv {
    async fn probe_service_version(&self) -> anyhow::Result<ServiceVersionReply> {
        let (home, binary, binding) = self.source()?;
        let requirement = tokio::task::spawn_blocking(move || {
            let core = binary.map_or_else(|| crate::process::service_core_source(&home), Ok)?;
            let name = core
                .file_name()
                .and_then(|name| name.to_str())
                .context("selected core has no file name")?
                .to_owned();
            let digest = zenclash_service::management::sha256_file(&core)?;
            Ok::<_, anyhow::Error>(zenclash_service::CoreRequirement {
                name,
                sha256: Some(digest),
            })
        })
        .await
        .context("selected-core verification worker did not finish")??;
        if self.client.runtime_descriptor().binding_generation() != binding {
            anyhow::bail!("selected service source changed during verification");
        }
        let response = NativeEnv::new(requirement, self.effects.clone())
            .probe_service_version()
            .await?;
        if self.client.runtime_descriptor().binding_generation() != binding {
            anyhow::bail!("selected service source changed during native observation");
        }
        Ok(response)
    }

    async fn trusted_install_evidence(&self) -> anyhow::Result<bool> {
        // This NativeEnv method does not inspect core artifacts. Keep its native
        // Unix residual-helper and SCM/launchd/systemd evidence rules unchanged.
        NativeEnv::new(
            zenclash_service::CoreRequirement {
                name: if cfg!(windows) {
                    "mihomo.exe"
                } else {
                    "mihomo"
                }
                .to_owned(),
                sha256: None,
            },
            self.effects.clone(),
        )
        .trusted_install_evidence()
        .await
    }

    fn is_elevated(&self) -> bool {
        self.elevated
    }
    fn set_pac_available(&self, available: bool) {
        self.effects.set_pac_available(available);
    }
    fn publish(&self, state: &RunState) {
        self.effects.publish(state);
    }
    fn run_privileged(&self, action: PendingAction) -> anyhow::Result<()> {
        self.effects.run_privileged(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MihomoEndpoint, default_pac_script};

    fn external() -> CoreSession {
        CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::default()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn external_pac_remains_usable_without_claiming_managed_ownership() {
        let session = external();
        let server = PacServer::default();
        session.attach_pac_server(server.clone());
        session.reconcile_run_state();
        assert!(server.is_available());
        assert_eq!(session.run_state().mode, RunningMode::NotRunning);
    }

    #[test]
    fn offline_pac_never_becomes_available_from_a_disconnected_client() {
        let session =
            CoreSession::open_offline(CoreKind::Mihomo, std::env::temp_dir(), None).unwrap();
        let server = PacServer::default();
        session.attach_pac_server(server.clone());
        session.reconcile_run_state();
        assert!(!server.is_available());
        assert_eq!(session.run_state().mode, RunningMode::NotRunning);
    }

    #[tokio::test]
    async fn per_session_health_and_operation_publication_reaches_every_clone() {
        let session = external();
        let cloned = session.clone();
        let mut updates = session.subscribe_run_state();
        session
            .run_state
            .store
            .observe(ServiceHealth::VersionMismatch);
        updates.changed().await.unwrap();
        assert_eq!(cloned.run_state().health, ServiceHealth::VersionMismatch);
        let operation = cloned.run_state.store.begin_operation().unwrap();
        updates.changed().await.unwrap();
        assert!(session.run_state().op_in_flight);
        drop(operation);
        updates.changed().await.unwrap();
        assert!(!session.run_state().op_in_flight);
    }

    #[tokio::test]
    async fn changing_binding_retires_old_health_and_pending_intent() {
        let session = external();
        session
            .run_state
            .store
            .observe(ServiceHealth::VersionMismatch);
        session
            .run_state
            .store
            .request_action(PendingAction::Reinstall);
        session
            .switch_to_direct(MihomoEndpoint::new("127.0.0.1:9091", "replacement"))
            .await
            .unwrap();
        let state = session.run_state();
        assert_eq!(state.health, ServiceHealth::Unknown);
        assert_eq!(state.pending, None);
        assert!(!state.sidecar_allowed);
    }

    #[test]
    fn nested_attempts_cannot_reopen_pac_when_only_one_has_settled() {
        let session = external();
        let server = PacServer::default();
        session.attach_pac_server(server.clone());
        let first = session.begin_core_run_attempt();
        let second = session.begin_core_run_attempt();
        drop(second);
        assert!(!server.is_available());
        drop(first);
        assert!(server.is_available());
    }

    #[test]
    fn immediate_shutdown_closes_external_pac_and_cannot_be_reopened_by_an_attempt() {
        let session = external();
        let server = PacServer::default();
        session.attach_pac_server(server.clone());
        let attempt = session.begin_core_run_attempt();
        session.request_shutdown();
        drop(attempt);
        assert!(!server.is_available());
        assert_eq!(session.run_state().mode, RunningMode::NotRunning);
    }

    fn request_pac(server: &PacServer) -> String {
        use std::io::{Read as _, Write as _};
        let mut stream = std::net::TcpStream::connect(server.status().unwrap().address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"GET /pac HTTP/1.1\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[tokio::test]
    async fn live_child_observation_and_real_stop_control_the_attached_http_endpoint() {
        let fixture =
            super::super::ownership_tests::ChildFixture::new("persistent-run-state-pac").await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        let server = PacServer::default();
        session.attach_pac_server(server.clone());
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        assert!(request_pac(&server).starts_with("HTTP/1.1 503"));
        session.reconcile_run_state();
        assert_eq!(session.run_state().mode, RunningMode::Sidecar);
        assert!(request_pac(&server).starts_with("HTTP/1.1 200"));
        session.maintain(CoreMaintenanceIntent::Stop).await.unwrap();
        assert!(!fixture.process.snapshot().running);
        assert_eq!(session.run_state().mode, RunningMode::NotRunning);
        assert!(request_pac(&server).starts_with("HTTP/1.1 503"));
        assert_eq!(server.status(), Some(status));
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bootstrap_local_choice_publishes_health_and_retires_attention() {
        let fixture =
            super::super::ownership_tests::ChildFixture::new("startup-sidecar-state").await;
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(fixture.process.clone()).unwrap(),
        )
        .unwrap();
        for health in [
            ServiceHealth::NotInstalled,
            ServiceHealth::VersionMismatch,
            ServiceHealth::Unavailable("service Start was refused".into()),
        ] {
            session.record_startup_service_health(health.clone());
            let state = session.run_state();
            assert_eq!(state.health, health);
            assert!(state.sidecar_allowed);
            assert!(!state.service_needs_attention());
            assert!(!state.service_usable());
        }
        session.run_state.store.observe(ServiceHealth::Ready);
        assert!(!session.run_state().sidecar_allowed);
        assert!(session.run_state().service_usable());
        session.shutdown().await.unwrap();
    }

    #[test]
    fn bootstrap_external_controller_cannot_acquire_service_capability() {
        let session = external();
        session.record_startup_service_health(ServiceHealth::Ready);
        assert_eq!(session.run_state().health, ServiceHealth::Unknown);
        assert!(!session.run_state().service_usable());
    }

    #[test]
    fn bootstrap_offline_repair_preserves_health_without_accepting_sidecar() {
        let session =
            CoreSession::open_offline(CoreKind::Mihomo, std::env::temp_dir(), None).unwrap();
        session.record_startup_service_health(ServiceHealth::VersionMismatch);
        let state = session.run_state();
        assert_eq!(state.health, ServiceHealth::VersionMismatch);
        assert_eq!(state.mode, RunningMode::NotRunning);
        assert!(!state.sidecar_allowed);
        assert!(state.service_needs_attention());
    }
}
